//! Command-line diagnostic for the Lian Li wireless dongle.

use lianli_wireless::discovery::{Device, Master, FANS_PER_GROUP};
use lianli_wireless::dongle::Dongle;
use lianli_wireless::frame::BROADCAST;
use lianli_wireless::groups::Tracker;
use lianli_wireless::{clock, heartbeat, process, speed};
use std::process::ExitCode;
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
";

const DEFAULT_POLLS: u32 = 5;
const DEFAULT_HOLD: u64 = 10;
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
