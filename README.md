# Sysmo

A terminal system monitor and software inventory for macOS, written in Rust.

Sysmo is a two-tab TUI built on [ratatui](https://ratatui.rs):

- **Monitor** — live CPU, memory, swap, load, and process data, plus
  Apple-silicon hardware telemetry: CPU/GPU/ANE power draw, E-cluster and
  P-cluster frequencies, GPU utilization, temperatures, and fan speeds.
  It also covers disk (used/free space on the boot volume and its SMART
  health), battery (charge, health, and cycle count, on Macs that have one),
  and network (throughput, totals, and the addressed interfaces).
- **Inventory** — a scan of installed software: `.app` bundles, Homebrew
  formulae and casks, common command-line tools, and language toolchains,
  each with a detected version where available.

## Requirements

- macOS
- A Rust toolchain with Cargo (edition 2024)

Hardware telemetry availability depends on the Mac and macOS version.
Unsupported power, frequency, GPU, temperature, or fan readings are shown as
unavailable while the rest of the monitor continues to work. No root or
special entitlements are required.

## Install

```sh
brew tap framicheli/sysmo
brew install sysmo
```

If Homebrew refuses to load the formula as an untrusted third-party tap, run
`brew trust framicheli/sysmo` first, then retry `brew install sysmo`.

Or build from source:

```sh
cargo install --path .
```

Or build and run in place:

```sh
cargo run --release
```

To build the binary without running it:

```sh
cargo build --release
./target/release/sysmo
```

## Configuration

There is no config file. One environment variable:

| Variable | Effect |
| --- | --- |
| `SYSMO_NO_FFI=1` | Disable the hardware-specific collectors (IOReport, SMC, IOKit GPU) and show only the standard system metrics. Useful for troubleshooting or unsupported hardware. |

```sh
SYSMO_NO_FFI=1 sysmo
```

## Controls

### Global

| Key | Action |
| --- | --- |
| `q`, `Esc` | Quit |
| `Tab`, `Shift-Tab`, arrow left/right | Switch view |
| `1`, `2` | Open Monitor or Inventory |
| `p` | Pause or resume metric updates |
| `j`/`k`, arrow down/up | Move selection |

### Monitor

| Key | Action |
| --- | --- |
| `c` | Sort processes by CPU; press again to reverse |
| `m` | Sort processes by memory; press again to reverse |
| `x` | Kill the selected process |

`x` sends the kill request immediately and does not ask for confirmation.
Success or failure is reported in the status line.

### Inventory

| Key | Action |
| --- | --- |
| `/` | Filter by name (case-insensitive); `Enter` accepts, `Esc` clears |
| `s` | Cycle through source filters: All → App → Brew → Cask → Tool → Language |
| `r` | Rescan installed software (ignored while a scan is running) |

While typing a filter, all other keys are captured as filter text.

## What the inventory scans

| Source | How it is found |
| --- | --- |
| App | `.app` bundles in `/Applications`, `/System/Applications`, and `~/Applications` (one level of nesting, e.g. `/Applications/Utilities`); versions read from each bundle's `Info.plist` |
| Brew | Directories under `<prefix>/Cellar` for both `/opt/homebrew` (Apple silicon) and `/usr/local` (Intel) |
| Cask | Directories under `<prefix>/Caskroom`, same prefixes |
| Tool / Language | A fixed registry of well-known binaries (git, docker, gh, node, python3, go, rustc, java, …) located on `PATH`; each present binary is probed with its `--version` flag under a 2-second timeout |

The scan runs on a background thread and streams results into the UI as they
arrive, so the list fills immediately and version numbers appear as probes
complete. The status line reports each category as it finishes and the total
scan duration at the end.

## Architecture

Three threads, connected by channels:

- **UI thread** (`main.rs`, `app.rs`, `ui.rs`) — event loop at a 1-second
  tick, event-driven rendering, all keyboard handling and state.
- **Metrics thread** (`metrics/sysinfo_collector.rs`) — samples CPU, memory,
  swap, load, and per-process data via the `sysinfo` crate once per second,
  merges in the latest hardware sample, and handles kill requests.
- **FFI thread** (`metrics/ffi/`) — polls Apple-silicon telemetry once per
  second through three independent collectors, each optional so a failure
  degrades gracefully:
  - `ioreport.rs` — power (CPU/GPU/ANE) and cluster frequencies via the
    private IOReport framework
  - `smc.rs` — temperatures and fan speeds via the SMC
  - `gpu.rs` — GPU utilization via IOKit

  The binding approach is derived from
  [macmon](https://github.com/vladkens/macmon) (MIT).

Dependencies are deliberately minimal: `ratatui` + `crossterm` for the UI,
`sysinfo` for standard metrics, `core-foundation` for the FFI, and `plist`
for app bundle versions.

## Test

```sh
cargo test
```

Unit tests cover key handling, sorting, filtering, and version parsing. A
real-system inventory smoke test is ignored by default; run it with:

```sh
cargo test -- --ignored --nocapture
```
