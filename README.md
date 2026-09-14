# lianli-wireless

Drives Lian Li UNI FAN wireless fans on Windows through the Lian Li RF
dongle (transmitter `0416:8040`, receiver `0416:8041`), so the fans can be
controlled without L-Connect running. The crate builds a native library
with a C interface and a `probe` command-line tool; a thin FanControl
plugin under `plugin/` loads the library. Fans only: lighting and screens
are left alone.

## Build

```
cargo build --release
cargo test
```
