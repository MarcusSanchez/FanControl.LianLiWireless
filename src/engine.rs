//! The fan loop: once a second, poll the receiver, send the heartbeat,
//! and send every group's duty when it is due.
//!
//! The engine owns the dongle for as long as it runs. Hosts hand it a
//! percentage per group and read a snapshot of what the groups report.
//! Safety lives here: no fan below the floor, and every reachable fan at
//! full speed while the dongle is lost and not yet back. A group that
//! goes quiet keeps its last duty, which the firmware holds on its own,
//! and is forgotten after a long silence. A stop leaves every group at
//! its last duty too.

use crate::discovery::{Reply, FANS_PER_GROUP};
use crate::dongle::{self, Dongle};
use crate::frame::{RfPayload, BROADCAST};
use crate::groups::Tracker;
use crate::heartbeat::{self, Clock, Readings};
use crate::{clock, speed};
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How often the loop runs.
pub const TICK: Duration = Duration::from_secs(1);

/// Lowest percentage a host can ask for. Anything lower is raised to it.
pub const FLOOR_PERCENT: u8 = 30;

/// What every reachable group is set to when something is lost.
pub const FAILSAFE_PERCENT: u8 = 100;

/// Polls that must fail in a row before the dongle counts as lost.
pub const POLL_FAILURES: u32 = 3;

/// Least time between two logged target changes on one group. Changes in
/// between are counted and mentioned on the next line.
pub const CHANGE_LOG_EVERY: Duration = Duration::from_secs(10);

/// Wait before the second reconnect attempt; each later one waits twice
/// as long, up to [`RECONNECT_MAX_WAIT`]. The first attempt is immediate.
pub const RECONNECT_FIRST_WAIT: Duration = Duration::from_secs(2);

/// Longest wait between reconnect attempts.
pub const RECONNECT_MAX_WAIT: Duration = Duration::from_secs(60);

/// How often a poll that skipped records is mentioned in the log.
const SKIP_LOG_EVERY: Duration = Duration::from_secs(60);

/// Least time between two logged lines for the same failure text.
pub const REPEAT_LOG_EVERY: Duration = Duration::from_secs(60);

/// The dongle as the engine needs it.
pub trait Link {
    /// The dongle's own address.
    fn master_mac(&self) -> [u8; 6];
    /// The channel the dongle is on.
    fn channel(&self) -> u8;
    /// One discovery poll.
    fn poll(&mut self) -> Result<Reply, dongle::Error>;
    /// One radio payload to a receiver type on a channel.
    fn send(&mut self, channel: u8, receiver: u8, payload: &RfPayload)
        -> Result<(), dongle::Error>;
    /// Closes the dongle and opens and connects it again. Without
    /// `every_channel` only the channel it was on is tried.
    fn reconnect(&mut self, every_channel: bool) -> Result<(), dongle::Error>;
}

impl Link for Dongle {
    fn master_mac(&self) -> [u8; 6] {
        self.master.master_mac
    }

    fn channel(&self) -> u8 {
        self.channel
    }

    fn poll(&mut self) -> Result<Reply, dongle::Error> {
        Dongle::poll(self)
    }

    fn send(
        &mut self,
        channel: u8,
        receiver: u8,
        payload: &RfPayload,
    ) -> Result<(), dongle::Error> {
        Dongle::send_on(self, channel, receiver, payload)
    }

    fn reconnect(&mut self, every_channel: bool) -> Result<(), dongle::Error> {
        Dongle::reopen(self, every_channel)
    }
}

/// Why every reachable group is being held at full speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alarm {
    /// Polls have failed [`POLL_FAILURES`] times in a row and the dongle
    /// has not been reconnected yet.
    Dongle,
}

impl fmt::Display for Alarm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dongle => write!(f, "{POLL_FAILURES} polls failed in a row"),
        }
    }
}

/// One group as the host sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupState {
    /// Address of the group's receiver.
    pub mac: [u8; 6],
    /// Receiver type.
    pub receiver: u8,
    /// Fans attached.
    pub fan_count: u8,
    /// Model byte of the first fan.
    pub model: u8,
    /// Whether the group has been heard recently.
    pub online: bool,
    /// Speed of each fan.
    pub rpm: [u16; FANS_PER_GROUP],
    /// Duty the receiver reports for each fan.
    pub duty: [u8; FANS_PER_GROUP],
    /// Duties last sent, if any.
    pub target: Option<[u8; FANS_PER_GROUP]>,
    /// Whether the receiver reports the target applied.
    pub acknowledged: bool,
    /// Sends of the target since it was last seen applied.
    pub unacknowledged: u32,
}

/// What the engine knows, as of its last tick.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    /// The dongle's address.
    pub master_mac: [u8; 6],
    /// The dongle's channel.
    pub channel: u8,
    /// Ticks run so far.
    pub ticks: u64,
    /// Polls that succeeded.
    pub polls: u64,
    /// Polls that failed.
    pub poll_failures: u64,
    /// Times the dongle was reconnected after being lost.
    pub reconnects: u64,
    /// Records in discovery replies that could not be read.
    pub skipped_records: u64,
    /// Whether the failsafe is in force, and why.
    pub alarm: Option<Alarm>,
    /// Text of the last error, if any.
    pub last_error: Option<String>,
    /// Fan groups bound to the dongle, in slot order.
    pub groups: Vec<GroupState>,
}

/// The loop's state between ticks.
pub struct Core {
    tracker: Tracker,
    channel: u8,
    wanted: HashMap<[u8; 6], u8>,
    heartbeat_sent: bool,
    last_heartbeat: Option<Instant>,
    failures_in_a_row: u32,
    reconnect_at: Option<Instant>,
    reconnect_wait: Duration,
    last_skip_log: Option<Instant>,
    last_online: Vec<[u8; 6]>,
    change_log: HashMap<[u8; 6], ChangeLog>,
    fail_log: HashMap<String, (Instant, u32)>,
    alarm: Option<Alarm>,
    snapshot: Snapshot,
    events: Vec<String>,
}

/// What the log has said about one group's target, and what it has not
/// said yet.
struct ChangeLog {
    logged_at: Instant,
    held: u32,
    pending: Option<String>,
}

impl Core {
    /// A core for a dongle with this address and channel.
    pub fn new(master_mac: [u8; 6], channel: u8) -> Self {
        Self {
            tracker: Tracker::new(master_mac),
            channel,
            wanted: HashMap::new(),
            heartbeat_sent: false,
            last_heartbeat: None,
            failures_in_a_row: 0,
            reconnect_at: None,
            reconnect_wait: RECONNECT_FIRST_WAIT,
            last_skip_log: None,
            last_online: Vec::new(),
            change_log: HashMap::new(),
            fail_log: HashMap::new(),
            alarm: None,
            snapshot: Snapshot {
                master_mac,
                channel,
                ..Snapshot::default()
            },
            events: Vec::new(),
        }
    }

    /// Asks for a percentage on a group. Takes effect on the next tick.
    pub fn want(&mut self, mac: [u8; 6], percent: u8) {
        self.wanted.insert(mac, percent.clamp(FLOOR_PERCENT, 100));
    }

    /// Stops driving a group. Nothing more is sent to it and its fans keep
    /// the duty they have.
    pub fn release(&mut self, mac: [u8; 6]) {
        let asked = self.wanted.remove(&mac).is_some();
        let driven = self.tracker.clear_target(&mac);
        if asked || driven {
            self.events.push(format!(
                "{} released; its fans keep their last duty",
                text(&mac)
            ));
        }
    }

    /// What the engine knows, as of the last tick.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// Takes the messages logged since the last call.
    pub fn take_events(&mut self) -> Vec<String> {
        std::mem::take(&mut self.events)
    }

    /// One pass of the loop, at `now`.
    pub fn tick<L: Link>(&mut self, link: &mut L, now: Instant, clock: Clock) {
        self.snapshot.ticks += 1;
        self.heartbeat(link, now, clock);
        self.poll(link, now);
        self.sweep(now);
        self.judge();
        self.drive(link, now);
        self.recover(link, now);
        self.flush_change_log(now);
        self.flush_fail_log(now);
        self.publish(now);
    }

    fn heartbeat<L: Link>(&mut self, link: &mut L, now: Instant, clock: Clock) {
        if self
            .last_heartbeat
            .is_some_and(|last| now.duration_since(last) < heartbeat::INTERVAL)
        {
            return;
        }
        let block = heartbeat::block(&Readings::default(), &clock);
        let payload = heartbeat::payload(&link.master_mac(), &block, !self.heartbeat_sent);
        match link.send(self.channel, BROADCAST, &payload) {
            Ok(()) => {
                self.heartbeat_sent = true;
                self.last_heartbeat = Some(now);
            }
            Err(error) => self.fail(format!("heartbeat: {error}"), now),
        }
    }

    fn poll<L: Link>(&mut self, link: &mut L, now: Instant) {
        match link.poll() {
            Ok(reply) => {
                self.snapshot.polls += 1;
                self.failures_in_a_row = 0;
                self.reconnect_at = None;
                self.reconnect_wait = RECONNECT_FIRST_WAIT;
                if reply.skipped > 0 {
                    self.snapshot.skipped_records += u64::from(reply.skipped);
                    if self
                        .last_skip_log
                        .is_none_or(|last| now.duration_since(last) >= SKIP_LOG_EVERY)
                    {
                        self.last_skip_log = Some(now);
                        self.events.push(format!(
                            "{} unreadable record(s) in a reply, {} so far",
                            reply.skipped, self.snapshot.skipped_records
                        ));
                    }
                }
                let before: Vec<[u8; 6]> = self.tracker.online().map(|g| g.device.mac).collect();
                self.tracker.observe(&reply, now);
                let master = self.tracker.master_mac();
                for group in self.tracker.online() {
                    if !before.contains(&group.device.mac) {
                        let whose = if group.bound_to(&master) {
                            ""
                        } else {
                            ", bound to another dongle"
                        };
                        self.events.push(format!(
                            "{} online: {} fans, receiver {}{whose}",
                            text(&group.device.mac),
                            group.device.fan_count,
                            group.device.receiver
                        ));
                    }
                }
            }
            Err(error) => {
                self.snapshot.poll_failures += 1;
                self.failures_in_a_row += 1;
                self.fail(format!("poll: {error}"), now);
            }
        }
    }

    /// Records a failure. The first of a kind is logged at once; the same
    /// text again within [`REPEAT_LOG_EVERY`] is counted and mentioned
    /// with its count once that time is up, so a dongle that stays lost
    /// costs one line a minute per kind of failure, not one a second.
    fn fail(&mut self, message: String, now: Instant) {
        self.snapshot.last_error = Some(message.clone());
        match self.fail_log.get_mut(&message) {
            Some((logged_at, held)) => {
                *held += 1;
                if now.duration_since(*logged_at) >= REPEAT_LOG_EVERY {
                    let count = std::mem::take(held);
                    *logged_at = now;
                    self.events
                        .push(format!("{message} (repeated {count} times)"));
                }
            }
            None => {
                self.fail_log.insert(message.clone(), (now, 0));
                self.events.push(message);
            }
        }
    }

    /// Writes the counts of failures held back by [`Self::fail`] whose
    /// time is up, and drops the kinds not seen for that long.
    fn flush_fail_log(&mut self, now: Instant) {
        let mut lines = Vec::new();
        self.fail_log.retain(|message, (logged_at, held)| {
            if now.duration_since(*logged_at) < REPEAT_LOG_EVERY {
                return true;
            }
            if *held > 0 {
                lines.push(format!("{message} (repeated {held} times)"));
            }
            false
        });
        self.events.extend(lines);
    }

    /// Once polls have failed [`POLL_FAILURES`] times in a row, tries to
    /// reconnect the dongle: at once the first time, then after a wait
    /// that doubles up to [`RECONNECT_MAX_WAIT`]. Runs after the groups
    /// have been driven, so the tick that trips the failsafe sends full
    /// speed on the handles it still has before giving them up. Attempts
    /// try the dongle's last channel only; once the wait has reached its
    /// cap they scan every channel, in case the dongle was swapped or
    /// paired anew while the engine ran.
    fn recover<L: Link>(&mut self, link: &mut L, now: Instant) {
        if self.failures_in_a_row < POLL_FAILURES {
            return;
        }
        if self.reconnect_at.is_some_and(|at| now < at) {
            return;
        }
        let every_channel = self.reconnect_wait >= RECONNECT_MAX_WAIT;
        match link.reconnect(every_channel) {
            Ok(()) => {
                self.channel = link.channel();
                self.snapshot.master_mac = link.master_mac();
                self.snapshot.channel = link.channel();
                self.snapshot.reconnects += 1;
                self.failures_in_a_row = 0;
                self.reconnect_at = None;
                self.reconnect_wait = RECONNECT_FIRST_WAIT;
                self.heartbeat_sent = false;
                self.last_heartbeat = None;
                self.events.push(format!(
                    "reconnected: master {} channel {}",
                    text(&link.master_mac()),
                    link.channel()
                ));
                self.judge();
            }
            Err(error) => {
                let scope = if every_channel {
                    "reconnect on every channel"
                } else {
                    "reconnect"
                };
                self.fail(
                    format!(
                        "{scope}: {error}; next try in {} s",
                        self.reconnect_wait.as_secs()
                    ),
                    now,
                );
                self.reconnect_at = Some(now + self.reconnect_wait);
                self.reconnect_wait = (self.reconnect_wait * 2).min(RECONNECT_MAX_WAIT);
            }
        }
    }

    /// Drops groups that have gone quiet from the slot order, logging
    /// each one the first time, and forgets those quiet for too long.
    fn sweep(&mut self, now: Instant) {
        let master = self.tracker.master_mac();
        let before = std::mem::take(&mut self.last_online);
        let forgotten = self.tracker.sweep(now);
        let after: Vec<[u8; 6]> = self.tracker.online().map(|g| g.device.mac).collect();
        self.last_online = after.clone();
        for mac in before {
            if !after.contains(&mac) && !forgotten.iter().any(|(gone, _)| *gone == mac) {
                let fans = self
                    .tracker
                    .group(&mac)
                    .is_some_and(|g| g.bound_to(&master) && g.has_fans());
                if fans {
                    self.events.push(format!(
                        "{} unheard for {} s; its fans keep their last duty",
                        text(&mac),
                        crate::groups::OFFLINE_AFTER.as_secs()
                    ));
                }
            }
        }
        for (mac, bound) in forgotten {
            if !bound {
                self.events.push(format!(
                    "{} gone; it was bound to another dongle",
                    text(&mac)
                ));
                continue;
            }
            let again = if self.wanted.contains_key(&mac) {
                "; driven at its last asked percentage if it returns"
            } else {
                ""
            };
            self.events.push(format!(
                "{} forgotten after {} s unheard{again}",
                text(&mac),
                crate::groups::FORGET_AFTER.as_secs()
            ));
        }
    }

    fn judge(&mut self) {
        let alarm = (self.failures_in_a_row >= POLL_FAILURES).then_some(Alarm::Dongle);
        if alarm != self.alarm {
            match &alarm {
                Some(why) => self.events.push(format!(
                    "failsafe: {why}; every reachable group to {FAILSAFE_PERCENT}%"
                )),
                None => self
                    .events
                    .push(String::from("failsafe cleared; targets resume")),
            }
            self.alarm = alarm;
        }
    }

    fn drive<L: Link>(&mut self, link: &mut L, now: Instant) {
        let macs: Vec<[u8; 6]> = self.tracker.fans().map(|g| g.device.mac).collect();
        for mac in macs {
            let percent = if self.alarm.is_some() {
                Some(FAILSAFE_PERCENT)
            } else {
                self.wanted.get(&mac).copied()
            };
            let Some(percent) = percent else {
                continue;
            };
            let Some((fan_count, previous)) = self
                .tracker
                .group(&mac)
                .map(|g| (usize::from(g.device.fan_count), g.target))
            else {
                continue;
            };
            let mut wanted = [0; FANS_PER_GROUP];
            for slot in wanted.iter_mut().take(fan_count) {
                *slot = speed::duty_from_percent(percent);
            }
            let Some(target) = self.tracker.set_target(&mac, wanted) else {
                continue;
            };
            if previous != Some(target) {
                self.log_change(
                    &mac,
                    percent,
                    &target[..fan_count.clamp(1, FANS_PER_GROUP)],
                    now,
                );
            }
            self.send_if_due(link, &mac, now);
        }
    }

    /// Logs a target change, at most one line per group per
    /// [`CHANGE_LOG_EVERY`]. A change inside that time is held; the next
    /// line says how many were held, and [`Self::flush_change_log`] writes
    /// the last held change once the time is up.
    fn log_change(&mut self, mac: &[u8; 6], percent: u8, target: &[u8], now: Instant) {
        let line = format!("{} target {percent}% {target:?}", text(mac));
        let held = match self.change_log.get_mut(mac) {
            Some(entry) => {
                if now.duration_since(entry.logged_at) < CHANGE_LOG_EVERY {
                    entry.held += 1;
                    entry.pending = Some(line);
                    return;
                }
                entry.logged_at = now;
                entry.pending = None;
                std::mem::take(&mut entry.held)
            }
            None => {
                self.change_log.insert(
                    *mac,
                    ChangeLog {
                        logged_at: now,
                        held: 0,
                        pending: None,
                    },
                );
                0
            }
        };
        self.events.push(line + &held_suffix(held));
    }

    fn flush_change_log(&mut self, now: Instant) {
        for entry in self.change_log.values_mut() {
            if entry.pending.is_none() || now.duration_since(entry.logged_at) < CHANGE_LOG_EVERY {
                continue;
            }
            let Some(line) = entry.pending.take() else {
                continue;
            };
            let held = std::mem::take(&mut entry.held);
            entry.logged_at = now;
            self.events
                .push(line + &held_suffix(held.saturating_sub(1)));
        }
    }

    /// Sends the group's target if the tracker says it is due. With the
    /// keepalive at one second and the tick at one second, that is every
    /// tick: the firmware wants each group's duty repeated that often.
    fn send_if_due<L: Link>(&mut self, link: &mut L, mac: &[u8; 6], now: Instant) {
        let Some(group) = self.tracker.group(mac) else {
            return;
        };
        if !group.due(now) {
            return;
        }
        let Some(target) = group.target else {
            return;
        };
        let device = group.device;
        let slot = self.tracker.slot(mac);
        // The payload names the master's channel and the frame around it
        // goes out on the device's own, the way the Lian Li driver
        // (`sgtaziz/lian-li-linux`) sends a speed command. Every device
        // seen so far reports the master's channel as its own.
        let payload = speed::payload(&device, &link.master_mac(), self.channel, slot, target);
        match link.send(device.channel, device.receiver, &payload) {
            Ok(()) => self.tracker.sent(mac, now),
            Err(error) => self.fail(format!("{}: {error}", text(mac)), now),
        }
    }

    fn publish(&mut self, now: Instant) {
        let master = self.tracker.master_mac();
        let mut groups: Vec<GroupState> = self
            .tracker
            .online()
            .filter(|g| g.bound_to(&master) && g.has_fans())
            .map(|g| GroupState {
                mac: g.device.mac,
                receiver: g.device.receiver,
                fan_count: g.device.fan_count,
                model: g.device.fan_types[0],
                online: true,
                rpm: g.device.rpm,
                duty: g.device.duty,
                target: g.target,
                acknowledged: g.acknowledged(now),
                unacknowledged: g.unacknowledged,
            })
            .collect();
        for g in self.tracker.groups() {
            if g.bound_to(&master) && g.has_fans() && !g.online(now) {
                groups.push(GroupState {
                    mac: g.device.mac,
                    receiver: g.device.receiver,
                    fan_count: g.device.fan_count,
                    model: g.device.fan_types[0],
                    online: false,
                    rpm: [0; FANS_PER_GROUP],
                    duty: g.device.duty,
                    target: g.target,
                    acknowledged: false,
                    unacknowledged: g.unacknowledged,
                });
            }
        }
        self.snapshot.alarm = self.alarm;
        self.snapshot.groups = groups;
    }

    /// Sends every group's current target one last time, so a target that
    /// arrived just before the stop is not lost, and records the stop. The
    /// groups keep whatever duty they have; the firmware holds it.
    pub fn shutdown<L: Link>(&mut self, link: &mut L, now: Instant) {
        let macs: Vec<[u8; 6]> = self.tracker.fans().map(|g| g.device.mac).collect();
        for mac in macs {
            let Some(group) = self.tracker.group(&mac) else {
                continue;
            };
            if group.target.is_none() || group.acknowledged(now) {
                continue;
            }
            let device = group.device;
            let target = group.target.unwrap_or_default();
            let slot = self.tracker.slot(&mac);
            let payload = speed::payload(&device, &link.master_mac(), self.channel, slot, target);
            if link.send(device.channel, device.receiver, &payload).is_ok() {
                self.tracker.sent(&mac, now);
            }
        }
        self.events
            .push(String::from("stopping; the groups keep their last duty"));
        self.publish(now);
    }
}

fn held_suffix(held: u32) -> String {
    match held {
        0 => String::new(),
        1 => String::from(" (1 earlier change not logged)"),
        n => format!(" ({n} earlier changes not logged)"),
    }
}

fn text(mac: &[u8; 6]) -> String {
    mac.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// What a host asks of the loop between ticks.
enum Request {
    Want([u8; 6], u8),
    Release([u8; 6]),
}

struct Shared {
    snapshot: Mutex<Snapshot>,
    requests: Mutex<Vec<Request>>,
    stop: AtomicBool,
}

/// The loop on its own thread.
pub struct Engine {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Engine {
    /// Finds and connects the dongle and starts the loop on it.
    pub fn open(log: impl FnMut(&str) + Send + 'static) -> Result<Self, dongle::Error> {
        let dongle = Dongle::open()?;
        Ok(Self::start(dongle, log))
    }

    /// Starts the loop on an already connected link.
    pub fn start<L: Link + Send + 'static>(
        mut link: L,
        mut log: impl FnMut(&str) + Send + 'static,
    ) -> Self {
        let shared = Arc::new(Shared {
            snapshot: Mutex::new(Snapshot {
                master_mac: link.master_mac(),
                channel: link.channel(),
                ..Snapshot::default()
            }),
            requests: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let worker = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name(String::from("lianli-wireless"))
            .spawn(move || {
                let mut core = Core::new(link.master_mac(), link.channel());
                log(&format!(
                    "engine started: master {} channel {}",
                    text(&link.master_mac()),
                    link.channel()
                ));
                let mut next = Instant::now();
                loop {
                    let started = Instant::now();
                    for request in std::mem::take(&mut *lock(&worker.requests)) {
                        match request {
                            Request::Want(mac, percent) => core.want(mac, percent),
                            Request::Release(mac) => core.release(mac),
                        }
                    }
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        core.tick(&mut link, started, clock::local());
                    }));
                    if let Err(panic) = outcome {
                        log(&format!("tick failed: {}", panic_text(&panic)));
                    }
                    for event in core.take_events() {
                        log(&event);
                    }
                    *lock(&worker.snapshot) = core.snapshot().clone();
                    if worker.stop.load(Ordering::Acquire) {
                        break;
                    }
                    next += TICK;
                    let now = Instant::now();
                    if next < now {
                        next = now;
                    }
                    loop {
                        let remaining = next.saturating_duration_since(Instant::now());
                        if remaining.is_zero() || worker.stop.load(Ordering::Acquire) {
                            break;
                        }
                        thread::sleep(remaining.min(Duration::from_millis(50)));
                    }
                    if worker.stop.load(Ordering::Acquire) {
                        break;
                    }
                }
                core.shutdown(&mut link, Instant::now());
                for event in core.take_events() {
                    log(&event);
                }
                *lock(&worker.snapshot) = core.snapshot().clone();
                log("engine stopped");
            })
            .expect("spawning the engine thread");
        Self {
            shared,
            thread: Some(thread),
        }
    }

    /// Asks for a percentage on a group. Applied on the next tick, never
    /// below [`FLOOR_PERCENT`].
    pub fn set_percent(&self, mac: [u8; 6], percent: u8) {
        lock(&self.shared.requests).push(Request::Want(mac, percent));
    }

    /// Stops driving a group on the next tick. Its fans keep the duty
    /// they have.
    pub fn clear(&self, mac: [u8; 6]) {
        lock(&self.shared.requests).push(Request::Release(mac));
    }

    /// What the engine knew at its last tick.
    pub fn snapshot(&self) -> Snapshot {
        lock(&self.shared.snapshot).clone()
    }

    /// Stops the loop. The groups keep their last duty.
    pub fn stop(mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The text a panic carried, if it was a string.
pub fn panic_text(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = panic.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = panic.downcast_ref::<String>() {
        text.clone()
    } else {
        String::from("unknown panic")
    }
}

/// A scripted dongle for tests of anything built on the engine.
#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use crate::discovery::Device;
    use crate::enumerate::Role;
    use crate::win::{WinError, ERROR_DEVICE_NOT_CONNECTED};
    use std::collections::VecDeque;

    pub(crate) const MASTER: [u8; 6] = [9; 6];
    pub(crate) const A: [u8; 6] = [1; 6];
    pub(crate) const B: [u8; 6] = [2; 6];

    pub(crate) fn device(mac: [u8; 6], receiver: u8, fans: u8, duty: u8) -> Device {
        let mut duties = [0; 4];
        let mut rpm = [0; 4];
        for slot in 0..usize::from(fans) {
            duties[slot] = duty;
            rpm[slot] = 1800;
        }
        Device {
            mac,
            master_mac: MASTER,
            channel: 8,
            receiver,
            device_type: 0,
            fan_count: fans,
            right_attach: false,
            effect: [0; 4],
            fan_types: [46, 46, 46, 0],
            rpm,
            duty: duties,
            sequence: 0,
            pwm_line: false,
            light_sync: false,
        }
    }

    pub(crate) fn reply(devices: &[Device]) -> Reply {
        Reply {
            reported: devices.len() as u8,
            masters: Vec::new(),
            devices: devices.to_vec(),
            skipped: 0,
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Sent {
        pub(crate) receiver: u8,
        pub(crate) command: u8,
        pub(crate) duty: [u8; 4],
    }

    pub(crate) struct Fake {
        replies: VecDeque<Result<Reply, dongle::Error>>,
        last: Reply,
        pub(crate) sent: Vec<Sent>,
        pub(crate) send_fails: bool,
        reconnects: VecDeque<Result<(), dongle::Error>>,
        pub(crate) reconnect_calls: u32,
        pub(crate) every_channel_calls: u32,
        /// Set by a failed reconnect, as the real dongle gives up its
        /// handles before trying to open again; cleared by a successful
        /// one. Every transfer fails meanwhile.
        pub(crate) closed: bool,
    }

    impl Fake {
        pub(crate) fn new(first: Reply) -> Self {
            Self {
                replies: VecDeque::new(),
                last: first,
                sent: Vec::new(),
                send_fails: false,
                reconnects: VecDeque::new(),
                reconnect_calls: 0,
                every_channel_calls: 0,
                closed: false,
            }
        }

        pub(crate) fn then(&mut self, reply: Result<Reply, dongle::Error>) {
            self.replies.push_back(reply);
        }

        pub(crate) fn then_reconnect(&mut self, outcome: Result<(), dongle::Error>) {
            self.reconnects.push_back(outcome);
        }

        pub(crate) fn heartbeats(&self) -> usize {
            self.sent.iter().filter(|s| s.command == 0x14).count()
        }

        pub(crate) fn speeds(&self) -> Vec<&Sent> {
            self.sent.iter().filter(|s| s.command == 0x10).collect()
        }
    }

    pub(crate) fn lost() -> dongle::Error {
        dongle::Error::Transfer(
            Role::Receiver,
            WinError {
                call: "WinUsb_ReadPipe",
                code: ERROR_DEVICE_NOT_CONNECTED,
            },
        )
    }

    impl Link for Fake {
        fn master_mac(&self) -> [u8; 6] {
            MASTER
        }

        fn channel(&self) -> u8 {
            8
        }

        fn poll(&mut self) -> Result<Reply, dongle::Error> {
            let next = self.replies.pop_front();
            if self.closed {
                return Err(dongle::Error::Closed);
            }
            match next {
                None => Ok(self.last.clone()),
                Some(Ok(reply)) => {
                    self.last = reply.clone();
                    Ok(reply)
                }
                Some(Err(error)) => Err(error),
            }
        }

        fn send(
            &mut self,
            channel: u8,
            receiver: u8,
            payload: &RfPayload,
        ) -> Result<(), dongle::Error> {
            if self.closed {
                return Err(dongle::Error::Closed);
            }
            if self.send_fails {
                return Err(lost());
            }
            assert_eq!(channel, 8);
            let mut duty = [0; 4];
            duty.copy_from_slice(&payload[17..21]);
            self.sent.push(Sent {
                receiver,
                command: payload[1],
                duty,
            });
            Ok(())
        }

        fn reconnect(&mut self, every_channel: bool) -> Result<(), dongle::Error> {
            self.reconnect_calls += 1;
            if every_channel {
                self.every_channel_calls += 1;
            }
            let outcome = self.reconnects.pop_front().unwrap_or(Ok(()));
            self.closed = outcome.is_err();
            outcome
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    fn clock() -> Clock {
        Clock::default()
    }

    #[test]
    fn first_tick_heartbeats_polls_and_sends_nothing_without_targets() {
        let base = Instant::now();
        let mut link = Fake::new(reply(&[device(A, 2, 3, 206), device(B, 6, 2, 206)]));
        let mut core = Core::new(MASTER, 8);
        core.tick(&mut link, base, clock());
        assert_eq!(link.heartbeats(), 1);
        assert!(link.speeds().is_empty());
        let snap = core.snapshot();
        assert_eq!(snap.ticks, 1);
        assert_eq!(snap.polls, 1);
        assert_eq!(snap.groups.len(), 2);
        assert!(snap.groups.iter().all(|g| g.online && g.target.is_none()));
        assert_eq!(snap.alarm, None);
        let events = core.take_events();
        assert!(events.iter().any(|e| e.contains("online")), "{events:?}");
    }

    #[test]
    fn heartbeat_goes_once_a_second_first_in_init_form() {
        let base = Instant::now();
        let mut link = Fake::new(reply(&[device(A, 2, 3, 206)]));
        let mut core = Core::new(MASTER, 8);
        core.tick(&mut link, base, clock());
        core.tick(&mut link, base + Duration::from_millis(500), clock());
        core.tick(&mut link, at(base, 1), clock());
        assert_eq!(link.heartbeats(), 2);
    }

    #[test]
    fn a_wanted_percent_is_floored_and_sent_each_tick() {
        let base = Instant::now();
        let mut link = Fake::new(reply(&[device(A, 2, 3, 206)]));
        let mut core = Core::new(MASTER, 8);
        core.want(A, 10);
        core.tick(&mut link, base, clock());
        let sent = link.speeds();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].receiver, 2);
        assert_eq!(sent[0].duty, [77, 77, 77, 0]);
        assert_eq!(core.snapshot().groups[0].target, Some([77, 77, 77, 0]));
        core.tick(&mut link, at(base, 1), clock());
        assert_eq!(link.speeds().len(), 2);
    }

    #[test]
    fn an_applied_target_is_refreshed_every_second() {
        let base = Instant::now();
        let mut link = Fake::new(reply(&[device(A, 2, 3, 206)]));
        let mut core = Core::new(MASTER, 8);
        core.want(A, 81);
        core.tick(&mut link, base, clock());
        assert_eq!(link.speeds()[0].duty, [207, 207, 207, 0]);
        core.tick(&mut link, base + Duration::from_millis(400), clock());
        assert_eq!(link.speeds().len(), 1);
        assert!(core.snapshot().groups[0].acknowledged);
        core.tick(&mut link, at(base, 1), clock());
        assert_eq!(link.speeds().len(), 2);
    }

    #[test]
    fn a_lost_group_is_logged_and_the_others_keep_their_targets() {
        let base = Instant::now();
        let a = device(A, 2, 3, 206);
        let b = device(B, 6, 2, 206);
        let mut link = Fake::new(reply(&[a, b]));
        let mut core = Core::new(MASTER, 8);
        core.want(A, 50);
        core.want(B, 50);
        core.tick(&mut link, base, clock());
        link.then(Ok(reply(&[a])));
        for second in 1..=15 {
            core.tick(&mut link, at(base, second), clock());
        }
        core.take_events();
        core.tick(&mut link, at(base, 16), clock());
        let snap = core.snapshot();
        assert_eq!(snap.alarm, None, "{snap:?}");
        assert_eq!(snap.groups.len(), 2);
        assert!(!snap.groups[1].online);
        assert_eq!(link.speeds().last().unwrap().duty, [128, 128, 128, 0]);
        let events = core.take_events();
        assert!(
            events
                .iter()
                .any(|e| e.contains("unheard") && e.contains("last duty")),
            "{events:?}"
        );
        core.tick(&mut link, at(base, 17), clock());
        assert!(core.take_events().is_empty());
        link.then(Ok(reply(&[a, b])));
        core.tick(&mut link, at(base, 18), clock());
        assert!(core.snapshot().groups.iter().all(|g| g.online));
        assert!(core.take_events().iter().any(|e| e.contains("online")));
        let to_b = link
            .speeds()
            .iter()
            .rev()
            .find(|s| s.receiver == 6)
            .unwrap()
            .duty;
        assert_eq!(to_b, [128, 128, 0, 0]);
    }

    #[test]
    fn a_group_quiet_for_ten_minutes_is_forgotten() {
        let base = Instant::now();
        let a = device(A, 2, 3, 206);
        let b = device(B, 6, 2, 206);
        let mut link = Fake::new(reply(&[a, b]));
        let mut core = Core::new(MASTER, 8);
        core.want(B, 50);
        core.tick(&mut link, base, clock());
        link.then(Ok(reply(&[a])));
        core.tick(&mut link, at(base, 599), clock());
        assert_eq!(core.snapshot().groups.len(), 2);
        core.take_events();
        core.tick(&mut link, at(base, 600), clock());
        let snap = core.snapshot();
        assert_eq!(snap.groups.len(), 1);
        assert_eq!(snap.groups[0].mac, A);
        let events = core.take_events();
        assert!(
            events.iter().any(|e| e.ends_with(
                "forgotten after 600 s unheard; driven at its last asked percentage if it returns"
            )),
            "{events:?}"
        );
        link.then(Ok(reply(&[a, b])));
        core.tick(&mut link, at(base, 601), clock());
        assert_eq!(core.snapshot().groups.len(), 2);
        assert_eq!(core.snapshot().groups[1].target, Some([128, 128, 0, 0]));
        assert_eq!(link.speeds().last().unwrap().receiver, 6);
    }

    #[test]
    fn a_device_bound_elsewhere_is_named_as_such_when_it_comes_and_goes() {
        let base = Instant::now();
        let a = device(A, 2, 3, 206);
        let mut foreign = device([7; 6], 2, 3, 206);
        foreign.master_mac = [8; 6];
        let mut link = Fake::new(reply(&[a, foreign]));
        let mut core = Core::new(MASTER, 8);
        core.tick(&mut link, base, clock());
        let events = core.take_events();
        assert!(
            events
                .iter()
                .any(|e| e
                    == "07:07:07:07:07:07 online: 3 fans, receiver 2, bound to another dongle"),
            "{events:?}"
        );
        assert!(events
            .iter()
            .any(|e| e == "01:01:01:01:01:01 online: 3 fans, receiver 2"));
        link.then(Ok(reply(&[a])));
        core.tick(&mut link, at(base, 16), clock());
        let events = core.take_events();
        assert_eq!(
            events,
            vec![String::from(
                "07:07:07:07:07:07 gone; it was bound to another dongle"
            )]
        );
        assert_eq!(core.snapshot().groups.len(), 1);
    }

    #[test]
    fn the_same_failure_is_logged_once_and_then_counted_once_a_minute() {
        let base = Instant::now();
        let mut link = Fake::new(reply(&[device(A, 2, 3, 206)]));
        let mut core = Core::new(MASTER, 8);
        core.want(A, 50);
        core.tick(&mut link, base, clock());
        link.then_reconnect(Ok(()));
        for second in 1..=130 {
            link.then(Err(lost()));
            core.tick(&mut link, at(base, second), clock());
        }
        let events = core.take_events();
        let polls: Vec<&String> = events.iter().filter(|e| e.starts_with("poll: ")).collect();
        assert_eq!(polls.len(), 3, "{polls:?}");
        assert!(!polls[0].contains("repeated"));
        assert!(polls[1].ends_with("(repeated 60 times)"), "{}", polls[1]);
        assert!(polls[2].ends_with("(repeated 60 times)"), "{}", polls[2]);
        assert!(core
            .snapshot()
            .last_error
            .as_deref()
            .unwrap()
            .starts_with("poll: "));
        assert!(!events.iter().any(|e| e.contains("closed")));
    }

    #[test]
    fn the_tripping_tick_sends_full_speed_before_the_handles_are_given_up() {
        let base = Instant::now();
        let mut link = Fake::new(reply(&[device(A, 2, 3, 206)]));
        let mut core = Core::new(MASTER, 8);
        core.want(A, 50);
        core.tick(&mut link, base, clock());
        link.then_reconnect(Err(lost()));
        for second in 1..=3 {
            link.then(Err(lost()));
            core.tick(&mut link, at(base, second), clock());
        }
        assert!(link.closed);
        assert_eq!(link.speeds().len(), 4);
        assert_eq!(link.speeds()[3].duty, [255, 255, 255, 0]);
        core.take_events();
        core.tick(&mut link, at(base, 4), clock());
        assert_eq!(link.speeds().len(), 4, "nothing goes out while closed");
        let events = core.take_events();
        assert!(
            events
                .iter()
                .any(|e| e.starts_with("poll: ") && e.contains("closed until it is reopened")),
            "{events:?}"
        );
        core.tick(&mut link, at(base, 5), clock());
        assert!(!link.closed, "the second attempt, two seconds on, succeeds");
        assert_eq!(core.snapshot().alarm, None);
        core.tick(&mut link, at(base, 6), clock());
        assert_eq!(link.speeds().last().unwrap().duty, [128, 128, 128, 0]);
    }

    #[test]
    fn once_the_wait_has_reached_a_minute_every_channel_is_scanned() {
        let base = Instant::now();
        let mut link = Fake::new(reply(&[device(A, 2, 3, 206)]));
        let mut core = Core::new(MASTER, 8);
        core.tick(&mut link, base, clock());
        for _ in 0..6 {
            link.then_reconnect(Err(lost()));
        }
        for second in 1..=64 {
            link.then(Err(lost()));
            core.tick(&mut link, at(base, second), clock());
        }
        assert_eq!(link.reconnect_calls, 5, "attempts at 3, 5, 9, 17 and 33 s");
        assert_eq!(link.every_channel_calls, 0);
        assert!(core
            .take_events()
            .iter()
            .any(|e| e.starts_with("reconnect: ") && e.contains("next try in 32 s")));
        core.tick(&mut link, at(base, 65), clock());
        assert_eq!(link.reconnect_calls, 6);
        assert_eq!(link.every_channel_calls, 1);
        assert!(core.take_events().iter().any(
            |e| e.starts_with("reconnect on every channel: ") && e.contains("next try in 60 s")
        ));
        core.tick(&mut link, at(base, 125), clock());
        assert_eq!(link.every_channel_calls, 2);
        assert_eq!(core.snapshot().reconnects, 1);
    }

    #[test]
    fn a_lost_dongle_is_reconnected_with_growing_waits() {
        let base = Instant::now();
        let a = device(A, 2, 3, 206);
        let mut link = Fake::new(reply(&[a]));
        let mut core = Core::new(MASTER, 8);
        core.want(A, 50);
        core.tick(&mut link, base, clock());
        for second in 1..=2 {
            link.then(Err(lost()));
            core.tick(&mut link, at(base, second), clock());
            assert_eq!(core.snapshot().alarm, None);
        }
        assert_eq!(link.reconnect_calls, 0);
        link.then_reconnect(Err(lost()));
        link.then_reconnect(Err(lost()));
        link.then_reconnect(Err(lost()));
        for second in 3..=16 {
            link.then(Err(lost()));
            core.tick(&mut link, at(base, second), clock());
        }
        assert_eq!(link.reconnect_calls, 3, "attempts at 3, 5 and 9 s");
        assert_eq!(core.snapshot().alarm, Some(Alarm::Dongle));
        assert_eq!(link.speeds().last().unwrap().duty, [255, 255, 255, 0]);
        let events = core.take_events();
        assert_eq!(
            events.iter().filter(|e| e.starts_with("failsafe:")).count(),
            1,
            "{events:?}"
        );
        assert!(events.iter().any(|e| e.contains("next try in 2 s")));
        assert!(events.iter().any(|e| e.contains("next try in 4 s")));
        assert!(events.iter().any(|e| e.contains("next try in 8 s")));
        link.then(Err(lost()));
        core.tick(&mut link, at(base, 17), clock());
        assert_eq!(link.reconnect_calls, 4);
        assert!(core.take_events().iter().any(|e| e.contains("reconnected")));
        assert_eq!(core.snapshot().reconnects, 1);
        assert_eq!(core.snapshot().alarm, None);
    }

    #[test]
    fn after_a_reconnect_the_heartbeat_restarts_in_init_form_and_targets_resume() {
        let base = Instant::now();
        let a = device(A, 2, 3, 206);
        let mut link = Fake::new(reply(&[a]));
        let mut core = Core::new(MASTER, 8);
        core.want(A, 50);
        core.tick(&mut link, base, clock());
        for second in 1..=3 {
            link.then(Err(lost()));
            core.tick(&mut link, at(base, second), clock());
        }
        assert_eq!(link.reconnect_calls, 1);
        assert_eq!(core.snapshot().reconnects, 1);
        assert_eq!(core.snapshot().alarm, None);
        let before = link.heartbeats();
        core.tick(&mut link, at(base, 4), clock());
        assert_eq!(link.heartbeats(), before + 1);
        assert_eq!(link.speeds().last().unwrap().duty, [128, 128, 128, 0]);
        assert_eq!(core.snapshot().polls, 2);
    }

    #[test]
    fn unreadable_records_are_counted_and_mentioned_once_a_minute() {
        let base = Instant::now();
        let a = device(A, 2, 3, 206);
        let mut noisy = reply(&[a]);
        noisy.skipped = 2;
        let mut link = Fake::new(noisy.clone());
        let mut core = Core::new(MASTER, 8);
        core.tick(&mut link, base, clock());
        core.tick(&mut link, at(base, 1), clock());
        assert_eq!(core.snapshot().skipped_records, 4);
        let events = core.take_events();
        assert_eq!(
            events.iter().filter(|e| e.contains("unreadable")).count(),
            1
        );
        core.tick(&mut link, at(base, 61), clock());
        assert_eq!(
            core.take_events()
                .iter()
                .filter(|e| e.contains("unreadable"))
                .count(),
            1
        );
    }

    #[test]
    fn three_failed_polls_trip_the_failsafe_until_the_dongle_is_back() {
        let base = Instant::now();
        let a = device(A, 2, 3, 206);
        let mut link = Fake::new(reply(&[a]));
        let mut core = Core::new(MASTER, 8);
        core.want(A, 50);
        core.tick(&mut link, base, clock());
        link.then(Err(lost()));
        link.then(Err(lost()));
        core.tick(&mut link, at(base, 1), clock());
        core.tick(&mut link, at(base, 2), clock());
        assert_eq!(core.snapshot().alarm, None);
        assert_eq!(core.snapshot().poll_failures, 2);
        link.then(Err(lost()));
        link.then_reconnect(Err(lost()));
        core.tick(&mut link, at(base, 3), clock());
        assert_eq!(core.snapshot().alarm, Some(Alarm::Dongle));
        assert_eq!(link.speeds().last().unwrap().duty, [255, 255, 255, 0]);
        assert!(core
            .snapshot()
            .last_error
            .as_deref()
            .unwrap()
            .starts_with("reconnect:"));
        let failsafe_lines = core
            .take_events()
            .iter()
            .filter(|e| e.starts_with("failsafe:"))
            .count();
        assert_eq!(failsafe_lines, 1);
        link.then(Err(lost()));
        core.tick(&mut link, at(base, 4), clock());
        assert_eq!(core.snapshot().alarm, Some(Alarm::Dongle));
        assert!(!core
            .take_events()
            .iter()
            .any(|e| e.starts_with("failsafe:")));
        link.then(Err(lost()));
        core.tick(&mut link, at(base, 5), clock());
        assert_eq!(core.snapshot().alarm, None);
        core.tick(&mut link, at(base, 6), clock());
        assert_eq!(link.speeds().last().unwrap().duty, [128, 128, 128, 0]);
    }

    #[test]
    fn send_failures_are_recorded_not_fatal() {
        let base = Instant::now();
        let mut link = Fake::new(reply(&[device(A, 2, 3, 206)]));
        link.send_fails = true;
        let mut core = Core::new(MASTER, 8);
        core.want(A, 50);
        core.tick(&mut link, base, clock());
        let snap = core.snapshot();
        assert!(snap.last_error.is_some());
        assert_eq!(snap.polls, 1);
        let events = core.take_events();
        assert!(
            events.iter().any(|e| e.starts_with("heartbeat:")),
            "{events:?}"
        );
        assert!(
            events.iter().any(|e| e.starts_with("01:01:01:01:01:01:")),
            "{events:?}"
        );
        link.send_fails = false;
        core.tick(&mut link, at(base, 1), clock());
        assert_eq!(link.heartbeats(), 1);
        assert_eq!(link.speeds().len(), 1);
    }

    #[test]
    fn target_changes_are_logged_at_most_once_in_ten_seconds_per_group() {
        let base = Instant::now();
        let mut link = Fake::new(reply(&[device(A, 2, 3, 206), device(B, 6, 2, 206)]));
        let mut core = Core::new(MASTER, 8);
        core.want(A, 50);
        core.want(B, 50);
        core.tick(&mut link, base, clock());
        let events = core.take_events();
        let targets: Vec<&String> = events.iter().filter(|e| e.contains(" target ")).collect();
        assert_eq!(targets.len(), 2);
        assert!(
            targets[0].ends_with("target 50% [128, 128, 128]"),
            "{}",
            targets[0]
        );
        assert!(
            targets[1].ends_with("target 50% [128, 128]"),
            "{}",
            targets[1]
        );

        for (i, percent) in [51u8, 52, 53].iter().enumerate() {
            core.want(A, *percent);
            core.tick(&mut link, at(base, 1 + i as u64), clock());
        }
        for second in 4..10 {
            core.tick(&mut link, at(base, second), clock());
        }
        assert!(core.take_events().iter().all(|e| !e.contains(" target ")));

        core.tick(&mut link, at(base, 10), clock());
        let events = core.take_events();
        let targets: Vec<&String> = events.iter().filter(|e| e.contains(" target ")).collect();
        assert_eq!(targets.len(), 1);
        assert!(
            targets[0].ends_with("target 53% [135, 135, 135] (2 earlier changes not logged)"),
            "{}",
            targets[0]
        );

        core.want(A, 55);
        core.tick(&mut link, at(base, 11), clock());
        core.want(A, 60);
        core.tick(&mut link, at(base, 20), clock());
        let events = core.take_events();
        let targets: Vec<&String> = events.iter().filter(|e| e.contains(" target ")).collect();
        assert_eq!(targets.len(), 1);
        assert!(
            targets[0].ends_with("target 60% [153, 153, 153] (1 earlier change not logged)"),
            "{}",
            targets[0]
        );
        assert_eq!(
            core.snapshot()
                .groups
                .iter()
                .find(|g| g.mac == A)
                .unwrap()
                .target,
            Some([153, 153, 153, 0])
        );
    }

    #[test]
    fn a_released_group_is_left_alone_and_the_others_are_still_driven() {
        let base = Instant::now();
        let mut link = Fake::new(reply(&[device(A, 2, 3, 206), device(B, 6, 2, 206)]));
        let mut core = Core::new(MASTER, 8);
        core.want(A, 50);
        core.want(B, 50);
        core.tick(&mut link, base, clock());
        assert_eq!(link.speeds().len(), 2);

        core.release(A);
        let events = core.take_events();
        assert!(
            events
                .iter()
                .any(|e| e.ends_with("released; its fans keep their last duty")),
            "{events:?}"
        );
        core.tick(&mut link, at(base, 1), clock());
        let sent = link.speeds();
        assert_eq!(sent.len(), 3);
        assert_eq!(sent[2].receiver, 6);
        let a = core.snapshot().groups.iter().find(|g| g.mac == A).unwrap();
        assert_eq!(a.target, None);
        assert!(!a.acknowledged);

        core.release(A);
        assert!(core.take_events().is_empty());

        core.want(A, 40);
        core.tick(&mut link, at(base, 2), clock());
        assert_eq!(link.speeds().len(), 5);
        assert_eq!(
            core.snapshot()
                .groups
                .iter()
                .find(|g| g.mac == A)
                .unwrap()
                .target,
            Some([102, 102, 102, 0])
        );
    }

    #[test]
    fn shutdown_resends_only_an_unconfirmed_target_and_leaves_the_rest() {
        let base = Instant::now();
        let a = device(A, 2, 3, 206);
        let b = device(B, 6, 2, 206);
        let mut link = Fake::new(reply(&[a, b]));
        let mut core = Core::new(MASTER, 8);
        core.want(A, 50);
        core.tick(&mut link, base, clock());
        assert_eq!(link.speeds().len(), 1);
        core.shutdown(&mut link, at(base, 1));
        let speeds = link.speeds();
        assert_eq!(speeds.len(), 2);
        assert!(speeds
            .iter()
            .all(|s| s.receiver == 2 && s.duty == [128, 128, 128, 0]));
        assert!(core.take_events().iter().any(|e| e.starts_with("stopping")));

        let mut applied = device(A, 2, 3, 128);
        applied.duty = [128, 128, 128, 0];
        let mut link = Fake::new(reply(&[applied, b]));
        let mut core = Core::new(MASTER, 8);
        core.want(A, 50);
        core.tick(&mut link, base, clock());
        core.shutdown(&mut link, at(base, 1));
        assert_eq!(link.speeds().len(), 1);
    }

    #[test]
    fn engine_thread_ticks_once_a_second_on_a_fixed_schedule() {
        let link = Fake::new(reply(&[device(A, 2, 3, 206)]));
        let engine = Engine::start(link, |_| {});
        thread::sleep(Duration::from_millis(3500));
        let ticks = engine.snapshot().ticks;
        assert!((3..=5).contains(&ticks), "{ticks} ticks in 3.5 s");
        engine.stop();
    }

    #[test]
    fn engine_thread_runs_ticks_and_stops() {
        let a = device(A, 2, 3, 206);
        let link = Fake::new(reply(&[a]));
        let logged = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&logged);
        let engine = Engine::start(link, move |line| lock(&sink).push(line.to_string()));
        engine.set_percent(A, 50);
        thread::sleep(Duration::from_millis(1300));
        let snap = engine.snapshot();
        assert!(snap.ticks >= 2, "{snap:?}");
        assert_eq!(snap.groups.len(), 1);
        assert_eq!(snap.groups[0].target, Some([128, 128, 128, 0]));
        engine.stop();
        let lines = lock(&logged).clone();
        assert!(
            lines.first().unwrap().starts_with("engine started"),
            "{lines:?}"
        );
        assert!(lines.iter().any(|l| l.starts_with("stopping")), "{lines:?}");
        assert_eq!(lines.last().unwrap(), "engine stopped");
    }
}
