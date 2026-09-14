//! Command-line diagnostic for the Lian Li wireless dongle.

use lianli_wireless::discovery::{Device, Master, FANS_PER_GROUP};
use lianli_wireless::dongle::Dongle;
use lianli_wireless::engine::{Engine, Snapshot};
use lianli_wireless::frame::BROADCAST;
use lianli_wireless::groups::Tracker;
use lianli_wireless::{clock, heartbeat, process, speed};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

const USAGE: &str = "usage: probe <command> [options]

commands:
  discover [--polls N] [--share]
      connect to the dongle, poll the receiver N times (default 5), and
      print every device heard. Refuses to run while the L-Connect
      service is running unless --share is given.

  set <group> <percent> [seconds]
      drive one fan group: send its current duty back and wait for the
      acknowledgement, then hold <percent> for <seconds> (default 10)
      while printing speeds, then restore the previous duty. <group> is
      the start of the group's address, enough to be unique. The
      heartbeat runs throughout. Refuses to run while the L-Connect
      service is running.

  run [minutes] [--every S] [--set <group>=<percent>]... [--leave]
      run the engine for <minutes> (default 60), printing its log as it
      happens and a status line every S seconds (default 10). Type
      '<group> <percent>' during the run to change a group, or 'q' to
      stop early. Afterwards the groups are put back to the duties they
      started at, or left at their last targets with --leave.
";

const DEFAULT_POLLS: u32 = 5;
const DEFAULT_HOLD: u64 = 10;
const DEFAULT_RUN_MINUTES: u64 = 60;
const DEFAULT_STATUS_EVERY: u64 = 10;
const POLL_GAP: Duration = Duration::from_secs(1);
const ACK_POLL_GAP: Duration = Duration::from_millis(250);
const ACK_LIMIT: Duration = Duration::from_secs(5);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None | Some("-h") | Some("--help") => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("discover") => discover(&args[1..]),
        Some("set") => set(&args[1..]),
        Some("run") => run(&args[1..]),
        Some(command) => {
            eprint!("probe: unknown command '{command}'\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("probe: {message}");
            ExitCode::FAILURE
        }
    }
}

fn refuse_if_lconnect_runs() -> Result<(), String> {
    if process::running(process::LCONNECT_SERVICE).map_err(|e| e.to_string())? {
        return Err(format!(
            "{} is running and owns the dongle; stop it first",
            process::LCONNECT_SERVICE
        ));
    }
    Ok(())
}

struct DiscoverOptions {
    polls: u32,
    share: bool,
}

fn parse_discover(args: &[String]) -> Result<DiscoverOptions, String> {
    let mut options = DiscoverOptions {
        polls: DEFAULT_POLLS,
        share: false,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--share" => options.share = true,
            "--polls" => {
                let value = args.next().ok_or("--polls needs a number")?;
                options.polls = value
                    .parse()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| format!("--polls: '{value}' is not a positive number"))?;
            }
            other => return Err(format!("unknown option '{other}'")),
        }
    }
    Ok(options)
}

fn discover(args: &[String]) -> Result<(), String> {
    let options = parse_discover(args)?;
    if !options.share {
        refuse_if_lconnect_runs().map_err(|e| format!("{e}, or pass --share to try anyway"))?;
    }
    let mut dongle = Dongle::open().map_err(|e| e.to_string())?;
    println!(
        "master {} channel {} firmware {}",
        mac(&dongle.master.master_mac),
        dongle.channel,
        dongle.master.firmware
    );
    for poll in 1..=options.polls {
        if poll > 1 {
            thread::sleep(POLL_GAP);
        }
        let reply = dongle.poll().map_err(|e| e.to_string())?;
        println!(
            "poll {poll}: {} reported, {} master, {} devices",
            reply.reported,
            reply.masters.len(),
            reply.devices.len()
        );
        for master in &reply.masters {
            println!("  {}", describe_master(master));
        }
        for device in &reply.devices {
            println!("  {}", describe(device));
        }
    }
    Ok(())
}

struct SetOptions {
    group: String,
    percent: u8,
    hold: Duration,
}

fn parse_set(args: &[String]) -> Result<SetOptions, String> {
    let group = args.first().ok_or("set needs a group address prefix")?;
    let percent = args.get(1).ok_or("set needs a percent")?;
    let percent: u8 = percent
        .parse()
        .ok()
        .filter(|p| *p <= 100)
        .ok_or_else(|| format!("'{percent}' is not a percent from 0 to 100"))?;
    let hold = match args.get(2) {
        None => DEFAULT_HOLD,
        Some(seconds) => seconds
            .parse()
            .ok()
            .filter(|s| *s > 0)
            .ok_or_else(|| format!("'{seconds}' is not a positive number of seconds"))?,
    };
    if args.len() > 3 {
        return Err(format!("unexpected argument '{}'", args[3]));
    }
    Ok(SetOptions {
        group: group.to_ascii_lowercase(),
        percent,
        hold: Duration::from_secs(hold),
    })
}

/// The dongle with the tracker and heartbeat that go with it.
struct Session {
    dongle: Dongle,
    tracker: Tracker,
    heartbeat_sent: bool,
    last_heartbeat: Option<Instant>,
}

impl Session {
    fn open() -> Result<Self, String> {
        let dongle = Dongle::open().map_err(|e| e.to_string())?;
        let tracker = Tracker::new(dongle.master.master_mac);
        Ok(Self {
            dongle,
            tracker,
            heartbeat_sent: false,
            last_heartbeat: None,
        })
    }

    /// Sends the heartbeat if a second has passed since the last one.
    fn heartbeat(&mut self) -> Result<(), String> {
        let now = Instant::now();
        if self
            .last_heartbeat
            .is_some_and(|last| now.duration_since(last) < heartbeat::INTERVAL)
        {
            return Ok(());
        }
        let block = heartbeat::block(&heartbeat::Readings::default(), &clock::local());
        let payload = heartbeat::payload(
            &self.dongle.master.master_mac,
            &block,
            !self.heartbeat_sent,
        );
        self.dongle
            .send(BROADCAST, &payload)
            .map_err(|e| format!("heartbeat: {e}"))?;
        self.heartbeat_sent = true;
        self.last_heartbeat = Some(now);
        Ok(())
    }

    fn poll(&mut self) -> Result<(), String> {
        let reply = self.dongle.poll().map_err(|e| e.to_string())?;
        self.tracker.observe(&reply, Instant::now());
        Ok(())
    }

    /// Sends the group's target if it is due, returning the duties sent.
    fn send_if_due(&mut self, mac: &[u8; 6]) -> Result<Option<[u8; FANS_PER_GROUP]>, String> {
        let now = Instant::now();
        let Some(group) = self.tracker.group(mac) else {
            return Err(format!("group {} vanished", self::mac(mac)));
        };
        if !group.due(now) {
            return Ok(None);
        }
        let device = group.device;
        let target = group.target.ok_or("no target set")?;
        let slot = self.tracker.slot(mac);
        let payload = speed::payload(
            &device,
            &self.dongle.master.master_mac,
            self.dongle.channel,
            slot,
            target,
        );
        self.dongle
            .send(device.receiver, &payload)
            .map_err(|e| e.to_string())?;
        self.tracker.sent(mac, now);
        Ok(Some(target))
    }

    /// Sets a target and keeps sending and polling until the group
    /// reports it applied, or the limit passes.
    fn apply(&mut self, mac: &[u8; 6], wanted: [u8; FANS_PER_GROUP], what: &str) -> Result<(), String> {
        let prepared = self
            .tracker
            .set_target(mac, wanted)
            .ok_or_else(|| format!("group {} unknown", self::mac(mac)))?;
        let slot = self.tracker.slot(mac);
        println!("{what}: sending {prepared:?} to slot {slot}");
        let started = Instant::now();
        let mut polls = 0;
        loop {
            self.heartbeat()?;
            self.send_if_due(mac)?;
            thread::sleep(ACK_POLL_GAP);
            self.poll()?;
            polls += 1;
            let group = self.tracker.group(mac).ok_or("group vanished")?;
            if group.acknowledged(Instant::now()) {
                println!(
                    "{what}: acknowledged after {polls} polls, duty {:?} rpm {:?}",
                    fans(&group.device.duty, group.device.fan_count),
                    fans(&group.device.rpm, group.device.fan_count)
                );
                return Ok(());
            }
            if started.elapsed() >= ACK_LIMIT {
                return Err(format!(
                    "{what}: not acknowledged within {} s; group reports duty {:?}",
                    ACK_LIMIT.as_secs(),
                    fans(&group.device.duty, group.device.fan_count)
                ));
            }
        }
    }

    /// Holds the current target for a while, printing what the group
    /// reports each second.
    fn hold(&mut self, mac: &[u8; 6], hold: Duration) -> Result<(), String> {
        let started = Instant::now();
        loop {
            self.heartbeat()?;
            self.send_if_due(mac)?;
            thread::sleep(POLL_GAP);
            self.poll()?;
            let now = Instant::now();
            let group = self.tracker.group(mac).ok_or("group vanished")?;
            println!(
                "  +{:>2}s duty {:?} rpm {:?}{}",
                now.duration_since(started).as_secs(),
                fans(&group.device.duty, group.device.fan_count),
                fans(&group.device.rpm, group.device.fan_count),
                if group.acknowledged(now) { "" } else { " (not acknowledged)" }
            );
            if now.duration_since(started) >= hold {
                return Ok(());
            }
        }
    }
}

fn set(args: &[String]) -> Result<(), String> {
    let options = parse_set(args)?;
    refuse_if_lconnect_runs()?;
    let mut session = Session::open()?;
    println!(
        "master {} channel {}",
        mac(&session.dongle.master.master_mac),
        session.dongle.channel
    );
    session.heartbeat()?;
    session.poll()?;
    let matching: Vec<Device> = session
        .tracker
        .fans()
        .map(|g| g.device)
        .filter(|d| mac(&d.mac).starts_with(&options.group))
        .collect();
    let device = match matching.as_slice() {
        [one] => *one,
        [] => {
            let known: Vec<String> = session.tracker.fans().map(|g| mac(&g.device.mac)).collect();
            return Err(format!(
                "no fan group starts with '{}'; groups: {}",
                options.group,
                known.join(", ")
            ));
        }
        several => {
            let names: Vec<String> = several.iter().map(|d| mac(&d.mac)).collect();
            return Err(format!("'{}' matches {}", options.group, names.join(", ")));
        }
    };
    let target = device.mac;
    println!("group {}", describe(&device));

    let before = device.duty;
    session.apply(&target, before, "echo")?;

    let mut wanted = [0; FANS_PER_GROUP];
    for slot in wanted.iter_mut().take(usize::from(device.fan_count)) {
        *slot = speed::duty_from_percent(options.percent);
    }
    let held = session
        .apply(&target, wanted, &format!("set {}%", options.percent))
        .and_then(|()| session.hold(&target, options.hold));
    let restored = session.apply(&target, before, "restore");
    held?;
    restored
}

struct RunOptions {
    length: Duration,
    every: Duration,
    initial: Vec<(String, u8)>,
    leave: bool,
}

fn parse_run(args: &[String]) -> Result<RunOptions, String> {
    let mut options = RunOptions {
        length: Duration::from_secs(DEFAULT_RUN_MINUTES * 60),
        every: Duration::from_secs(DEFAULT_STATUS_EVERY),
        initial: Vec::new(),
        leave: false,
    };
    let mut args = args.iter();
    let mut minutes_given = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--leave" => options.leave = true,
            "--every" => {
                let value = args.next().ok_or("--every needs a number of seconds")?;
                let seconds: u64 = value
                    .parse()
                    .ok()
                    .filter(|s| *s > 0)
                    .ok_or_else(|| format!("--every: '{value}' is not a positive number"))?;
                options.every = Duration::from_secs(seconds);
            }
            "--set" => {
                let value = args.next().ok_or("--set needs <group>=<percent>")?;
                options.initial.push(parse_assignment(value)?);
            }
            other if !minutes_given && !other.starts_with("--") => {
                let minutes: u64 = other
                    .parse()
                    .ok()
                    .filter(|m| *m > 0)
                    .ok_or_else(|| format!("'{other}' is not a positive number of minutes"))?;
                options.length = Duration::from_secs(minutes * 60);
                minutes_given = true;
            }
            other => return Err(format!("unexpected argument '{other}'")),
        }
    }
    Ok(options)
}

fn parse_assignment(text: &str) -> Result<(String, u8), String> {
    let (group, percent) = text
        .split_once('=')
        .ok_or_else(|| format!("'{text}' is not <group>=<percent>"))?;
    let percent: u8 = percent
        .parse()
        .ok()
        .filter(|p| *p <= 100)
        .ok_or_else(|| format!("'{percent}' is not a percent from 0 to 100"))?;
    Ok((group.to_ascii_lowercase(), percent))
}

fn stamp() -> String {
    let now = clock::local();
    format!("{:02}:{:02}:{:02}", now.hour, now.minute, now.second)
}

fn run(args: &[String]) -> Result<(), String> {
    let options = parse_run(args)?;
    refuse_if_lconnect_runs()?;
    let engine = Engine::open(|line| println!("{} | {line}", stamp())).map_err(|e| e.to_string())?;

    let started = Instant::now();
    let mut first: Option<Snapshot> = None;
    while first.is_none() {
        thread::sleep(Duration::from_millis(200));
        let snapshot = engine.snapshot();
        if snapshot.ticks > 0 {
            first = Some(snapshot);
        }
        if started.elapsed() > Duration::from_secs(10) {
            return Err(String::from("the engine did not tick within 10 s"));
        }
    }
    let first = first.unwrap_or_default();
    let starting: Vec<([u8; 6], [u8; FANS_PER_GROUP], u8)> = first
        .groups
        .iter()
        .map(|g| (g.mac, g.duty, g.fan_count))
        .collect();
    println!(
        "{} | {} groups; starting duties {}",
        stamp(),
        starting.len(),
        starting
            .iter()
            .map(|(m, d, n)| format!("{} {:?}", mac(m), fans(d, *n)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for (prefix, percent) in &options.initial {
        match resolve(&first, prefix) {
            Ok(target) => engine.set_percent(target, *percent),
            Err(message) => println!("{} | {message}", stamp()),
        }
    }

    let (lines, _reader) = stdin_lines();
    let mut last_status = Instant::now() - options.every;
    let mut quit = false;
    while started.elapsed() < options.length && !quit {
        thread::sleep(Duration::from_millis(250));
        while let Ok(line) = lines.try_recv() {
            let line = line.trim().to_ascii_lowercase();
            if line == "q" || line == "quit" {
                quit = true;
                break;
            }
            let snapshot = engine.snapshot();
            match line.split_once(' ') {
                Some((prefix, percent)) => match (resolve(&snapshot, prefix), percent.trim().parse::<u8>()) {
                    (Ok(target), Ok(percent)) if percent <= 100 => {
                        engine.set_percent(target, percent);
                        println!("{} | you asked for {}% on {}", stamp(), percent, mac(&target));
                    }
                    (Err(message), _) => println!("{} | {message}", stamp()),
                    _ => println!("{} | '{percent}' is not a percent from 0 to 100", stamp()),
                },
                None if line.is_empty() => {}
                None => println!("{} | type '<group> <percent>' or 'q'", stamp()),
            }
        }
        if last_status.elapsed() >= options.every {
            last_status = Instant::now();
            print_status(&engine.snapshot(), started.elapsed());
        }
    }

    println!("{} | stopping the engine", stamp());
    let last = engine.snapshot();
    engine.stop();
    let wanted: Vec<([u8; 6], [u8; FANS_PER_GROUP])> = if options.leave {
        println!("{} | confirming the last targets", stamp());
        last.groups
            .iter()
            .filter_map(|g| g.target.map(|t| (g.mac, t)))
            .collect()
    } else {
        println!("{} | putting the groups back", stamp());
        starting.iter().map(|(m, d, _)| (*m, *d)).collect()
    };
    let mut session = Session::open()?;
    session.heartbeat()?;
    session.poll()?;
    let mut failures = Vec::new();
    for (target, duty) in &wanted {
        if let Err(message) = session.apply(target, *duty, &format!("confirm {}", mac(target))) {
            failures.push(message);
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn resolve(snapshot: &Snapshot, prefix: &str) -> Result<[u8; 6], String> {
    let matching: Vec<[u8; 6]> = snapshot
        .groups
        .iter()
        .map(|g| g.mac)
        .filter(|m| mac(m).starts_with(prefix))
        .collect();
    match matching.as_slice() {
        [one] => Ok(*one),
        [] => Err(format!(
            "no group starts with '{prefix}'; groups: {}",
            snapshot.groups.iter().map(|g| mac(&g.mac)).collect::<Vec<_>>().join(", ")
        )),
        several => Err(format!(
            "'{prefix}' matches {}",
            several.iter().map(mac).collect::<Vec<_>>().join(", ")
        )),
    }
}

fn print_status(snapshot: &Snapshot, elapsed: Duration) {
    let minutes = elapsed.as_secs() / 60;
    let seconds = elapsed.as_secs() % 60;
    println!(
        "{} | {minutes:>3}:{seconds:02} ticks {} polls {} failed {}{}",
        stamp(),
        snapshot.ticks,
        snapshot.polls,
        snapshot.poll_failures,
        match &snapshot.alarm {
            Some(why) => format!(" FAILSAFE ({why})"),
            None => String::new(),
        }
    );
    for g in &snapshot.groups {
        println!(
            "{} |   {} {} duty {:?} target {} rpm {:?}{}",
            stamp(),
            mac(&g.mac),
            if g.online { "online " } else { "OFFLINE" },
            fans(&g.duty, g.fan_count),
            match g.target {
                Some(t) => format!("{:?}{}", fans(&t, g.fan_count), if g.acknowledged { "" } else { " (unacknowledged)" }),
                None => String::from("none"),
            },
            fans(&g.rpm, g.fan_count),
            if g.unacknowledged > 1 {
                format!(" sent {} times without confirmation", g.unacknowledged)
            } else {
                String::new()
            }
        );
    }
}

fn stdin_lines() -> (Receiver<String>, thread::JoinHandle<()>) {
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut line = String::new();
        loop {
            line.clear();
            match stdin.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if sender.send(line.clone()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    (receiver, reader)
}

fn fans<T: Copy>(values: &[T; FANS_PER_GROUP], count: u8) -> Vec<T> {
    values[..usize::from(count).clamp(1, FANS_PER_GROUP)].to_vec()
}

fn mac(mac: &[u8; 6]) -> String {
    mac.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn describe_master(master: &Master) -> String {
    format!("master {} channel {}", mac(&master.mac), master.channel)
}

fn describe(d: &Device) -> String {
    let attach = if d.right_attach { " right-attach" } else { "" };
    let bound = if d.master_mac == [0; 6] {
        String::from("unbound")
    } else {
        format!("bound to {}", mac(&d.master_mac))
    };
    format!(
        "{} rx {} ch {} type {} fans {}{} rpm {:?} duty {:?} ({}%) seq {} model {:?} {}",
        mac(&d.mac),
        d.receiver,
        d.channel,
        d.device_type,
        d.fan_count,
        attach,
        fans(&d.rpm, d.fan_count),
        fans(&d.duty, d.fan_count),
        speed::percent_from_duty(d.duty[0]),
        d.sequence,
        d.fan_types,
        bound
    )
}
