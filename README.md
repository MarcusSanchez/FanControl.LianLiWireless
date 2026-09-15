# lianli-wireless

Drives Lian Li UNI FAN wireless fans on Windows through the Lian Li RF
dongle (transmitter `0416:8040`, receiver `0416:8041`), so the fans can be
controlled without L-Connect running. The crate builds a native library
with a C interface and a `probe` command-line tool; a thin FanControl
plugin under `plugin/` loads the library. Fans only: lighting and screens
are left alone.

## What you need

- Windows, 64-bit. The dongles sit on the inbox WinUSB driver, which they
  bind to on their own; no driver install.
- The L-Connect service stopped whenever the plugin or the probe runs.
  Windows gives a WinUSB device to one process at a time, and the service
  holds the dongle for as long as it is up. Both the plugin and the probe
  refuse to start while `L-Connect-Service.exe` is running and say so.
  L-Connect's desktop app may stay open; it does not hold the dongle.
- For the plugin: FanControl, with its plugin support.

## The plugin

Two files go into FanControl's `Plugins` folder, side by side:

```
FanControl.LianLiWireless.dll
lianli_wireless.dll
```

FanControl loads the first at start; it loads the second from the same
folder. The plugin then finds the dongle, lists every fan group bound to
it, and registers one control per group and one speed sensor per fan.
Controls are named after the group's address, for example
`Wireless 7c:9c:06 (3 fans)`, and their identifiers carry the full address
so curve bindings survive restarts.

Behaviour worth knowing:

- Every group gets the plugin's heartbeat once a second, which the fan
  firmware needs; without it the fans run on their own with occasional
  bursts of speed. The heartbeat also carries the readings that fans with
  screens display, and the plugin sends none, so those screens show zero
  for temperature and load while it runs.
- A control value below 30 % is raised to 30 % by the plugin's engine, so
  fans never stop under FanControl. The library itself sends whatever it
  is given; `probe set` will send 0.
- If the dongle stops answering, or a group goes unheard for 15 seconds,
  every group still reachable is set to 100 % until things recover.
- When FanControl closes or refreshes the plugin, the groups keep whatever
  duty they had; the firmware holds it.
- The plugin writes a log to `%ProgramData%\FanControl\lianli-wireless.log`:
  groups coming and going, targets, failsafe changes, errors.

## The probe

`probe.exe` is a command-line diagnostic that uses the same code as the
plugin.

```
probe discover [--polls N] [--share]
probe set <group> <percent> [seconds]
probe run [minutes] [--every S] [--set <group>=<percent>]... [--leave]
```

`discover` connects, polls the receiver a few times and prints every device
it hears with its speeds and duties. `set` drives one group to a
percentage for a while and puts it back; `<group>` is the start of its
address, enough to be unique. `run` runs the whole engine from the console
for a soak, printing its log and a status block, taking `<group> <percent>`
lines typed during the run, and either restoring the starting duties at the
end or leaving the last targets in place with `--leave`.

## The C interface

`include/lianli_wireless.h` declares the seven functions the library
exports: open, close, set a percentage, read a fixed-layout state, take a
log line, read the last error, and the interface version. Every function
returns 0 or a negative code and never lets a panic cross into the caller.

## Build

```
cargo build --release
cargo test
cd plugin
dotnet build FanControl.LianLiWireless/FanControl.LianLiWireless.csproj -c Release
dotnet test FanControl.LianLiWireless.Tests/FanControl.LianLiWireless.Tests.csproj -c Release
```

The Rust side has no dependencies and builds with the GNU toolchain; the
plugin targets `netstandard2.0` and compiles against the
`FanControl.Plugins.dll` in FanControl's install folder, which it never
ships.
