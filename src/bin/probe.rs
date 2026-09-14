//! Command-line diagnostic for the Lian Li wireless dongle.

use std::process::ExitCode;

const USAGE: &str = "usage: probe <command>\n";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        None | Some("-h") | Some("--help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(command) => {
            eprint!("probe: unknown command '{command}'\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
