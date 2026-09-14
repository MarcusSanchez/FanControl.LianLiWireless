//! Command-line diagnostic for the Lian Li wireless dongle.

use lianli_wireless::discovery::{Device, Master};
use lianli_wireless::dongle::Dongle;
use lianli_wireless::process;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

const USAGE: &str = "usage: probe <command> [options]

commands:
  discover [--polls N] [--share]
      connect to the dongle, poll the receiver N times (default 5), and
      print every device heard. Refuses to run while the L-Connect
      service is running unless --share is given.
";

const DEFAULT_POLLS: u32 = 5;
const POLL_GAP: Duration = Duration::from_secs(1);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("-h") | Some("--help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("discover") => match discover(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("probe: {message}");
                ExitCode::FAILURE
            }
        },
        Some(command) => {
            eprint!("probe: unknown command '{command}'\n{USAGE}");
            ExitCode::from(2)
        }
    }
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
    if !options.share && process::running(process::LCONNECT_SERVICE).map_err(|e| e.to_string())? {
        return Err(format!(
            "{} is running and owns the dongle; stop it, or pass --share to try anyway",
            process::LCONNECT_SERVICE
        ));
    }
    let mut dongle = Dongle::open().map_err(|e| e.to_string())?;
    println!(
        "master {} channel {} firmware {}.{}",
        mac(&dongle.master.master_mac),
        dongle.channel,
        dongle.master.firmware >> 8,
        dongle.master.firmware & 0xFF
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
    let fans = usize::from(d.fan_count);
    let attach = if d.right_attach { " right-attach" } else { "" };
    let bound = if d.master_mac == [0; 6] {
        String::from("unbound")
    } else {
        format!("bound to {}", mac(&d.master_mac))
    };
    format!(
        "{} rx {} ch {} type {} fans {}{} rpm {:?} duty {:?} seq {} model {:?} {}",
        mac(&d.mac),
        d.receiver,
        d.channel,
        d.device_type,
        d.fan_count,
        attach,
        &d.rpm[..fans.clamp(1, 4)],
        &d.duty[..fans.clamp(1, 4)],
        d.sequence,
        d.fan_types,
        bound
    )
}
