# Development

## Layout

```
crates/battery-core   pure Rust: filters, curve, priors, estimator, scoring,
                      persistence, report seeding, alerts, pacing, glyphs.
                      No Win32 — this is where the tests live.
crates/battery-win    battery IOCTLs, CPU load
crates/battery-tray   tray icon, popup panel, settings window, message loop
```

The split is deliberate: everything that can be tested without a window is in
`battery-core`, which is why the maths has real coverage while the Win32 layer
stays thin enough to check by eye.

## Building and testing

```sh
cargo build --release      # target/release/battery-tray.exe
cargo test --workspace
```

Nothing beyond a stable Rust toolchain is required. Resource embedding wants the
Windows SDK's `rc.exe`; without it the build still succeeds and the app still
runs, losing only its Explorer icon, themed controls and version block. That is
deliberate — it must not break a local build — which is why the release workflow
checks separately that the version block made it into the binary.

## Debugging

```sh
BATTERY_TRAY_LOG=1 ./target/release/battery-tray.exe
```

Appends a trace of tray and activation events to
`%LOCALAPPDATA%\BatteryTray\trace.log`. Tray flyout dismissal depends on the
interleaving of activation and tray-callback messages, which is far easier to
observe than to reason about.

`--show-panel` and `--settings` open those windows directly.

If the app ever dies unexpectedly it writes `%LOCALAPPDATA%\BatteryTray\panic.log`.
A windowed process has no console, so without it a panic inside a window
procedure disappears and Windows reports only a fault in whichever system DLL
made the call.

## Measuring accuracy

```sh
cargo run --release -p battery-core --example replay -- trace.csv [--seed report.xml]
```

Replays a recorded trace and reports end-to-end error against a naive
`remaining ÷ current rate` baseline. See [prediction.md](prediction.md) for the
numbers this produces.

## Files on disk

State lives in `%LOCALAPPDATA%\BatteryTray\`:

| File | What it holds |
|---|---|
| `model.json` | Learned curve, priors, corrections. Discarded if written by a different version — stale beliefs are invisible once stored as numbers. |
| `settings.json` | Preferences. Repaired field by field rather than discarded, and tolerant of a byte-order mark, so hand editing is safe. |
| `history.bin` | 24 h of samples for the graph. |
| `panic.log` | Written only if the app dies unexpectedly. |

"Reset learned data" clears the model and re-seeds from Windows' own battery
report. It leaves settings alone.

## Cost

Measured while running: **~2.5 MB private working set**, ~0.05 s CPU per
minute, a ~530 KB executable, no runtime dependency.

Sampling blocks in the kernel via `BATTERY_WAIT_STATUS` and wakes on real
change or a 5-second timeout, with a fallback to timed polling if a driver
ignores the wait. It runs at 1 s while the panel is open and 5 s when it is
not, since a fresher reading is only worth anything on screen.

## Releases

Two kinds of build, and the About tab says which one you have:

- A **release** build is made from a tag by
  [the workflow](../.github/workflows/release.yml) and names itself after that
  tag — `v1.2.0` shows as `v1.2.0`.
- A **development** build is anything else, and names itself after the commit
  it came from: `0.1.1-dev (dfd407e)`.

So a binary someone sends you can be traced back to the source it was built
from, which a bare crate version shared by every local build cannot do.

Cutting a release is pushing a tag:

```sh
# bump version in Cargo.toml, add a CHANGELOG.md section, then:
git tag v1.2.0 && git push origin v1.2.0
```

The workflow runs the tests, builds, checks that the tag actually reached the
executable's version resource, and publishes the exe with its SHA-256. Release
notes come from that version's section of [CHANGELOG.md](../CHANGELOG.md),
falling back to a generated commit list only if there is no section for the tag.
