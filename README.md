# Battery Time Remaining

A lightweight Windows tray app that predicts battery time to empty, to 80% while
charging, and to full — and shows the watts actually entering or leaving the
battery.

```
 ┌─────────────────────────────┐
 │ 94.53%                      │
 │ 6.6 W  in      36.7/38.8 Wh │
 │ ─────────────────────────── │
 │ Until full              16m │
 │      ╭───╮       ╭───────── │  green above zero
 │ ─────╯   ╰─╮ ╭───╯          │  red below
 │            ╰─╯              │
 └─────────────────────────────┘
```

## Running it

```sh
cargo build --release
./target/release/battery-tray.exe
```

**Left-click** the tray icon to open the panel; click it again to close it.
Clicking any other window closes it too. **Right-click** for Settings and Quit;
everything else lives in Settings. Resting the pointer on the time row for a
moment shows the uncertainty range and confidence beside the cursor; any
movement dismisses it. Clicking the graph switches between its two kinds.

Flags: `--show-panel` and `--settings` open those directly.
Set `BATTERY_TRAY_LOG=1` to append a trace of tray and activation events to
`%LOCALAPPDATA%\BatteryTray\trace.log` — tray flyout dismissal depends on the
interleaving of activation and tray-callback messages, which is far easier to
observe than to reason about.

## Settings

A plain tabbed window built from standard Windows controls.

| Tab | Contents |
|---|---|
| General | Start with Windows · decimal places · keep the panel open |
| Display | Tray icon: battery preview / percentage / wattage / time / logo. Graph: kind, whether to show it at all, the zero line, and single-direction fitting. Panel theme: follow system / dark / light |
| Alerts | Low and critical warnings with their levels, plus optional 80% and fully-charged notifications |
| Battery | Full-charge vs design capacity, health, charge cycles, chemistry |
| Learning | What the model has learned, and Reset learned data |

A pinned panel stays put until dismissed and can be dragged anywhere on the
screen; it reopens where it was left. An unpinned one is anchored to the tray
icon, grows a small callout tail pointing back at it, and cannot be dragged —
it belongs to the icon it came from. Alerts latch when they fire and only rearm
once the charge has clearly moved away from the threshold, so a battery resting
on the warning level cannot produce a stream of them.

The settings window is deliberately not custom-drawn. The panel has to be — it
is a chart — but settings are a form, and real `BUTTON`, `STATIC` and
`SysTabControl32` controls inherit theming, keyboard navigation, focus rings,
high-contrast modes and screen-reader support for free.

The charge gauge only reports whole 10 mWh steps — about 0.026% on this pack —
so a two-decimal readout taken straight from it lurches. The panel dead-reckons
between steps from the measured power flow, which is precisely the quantity
that says how fast the true value is moving, and never drifts more than one
step from the last real reading. The result is held monotonic in the direction
the battery is actually going, so it cannot tick backwards while draining.
Measured on a charging pack: 87.61% → 87.65% → 87.71% over nine seconds.

Percentages in the *tray* stay whole even with decimals enabled. Legibility at
20 px is set by how many glyphs must fit, so time is stacked as two lines —
hours over minutes, like a clock — rather than squeezed into `2:45`, and the
finer percentage reading lives in the panel where there is room for it.

## How it predicts

**Power comes from the battery, not from guesswork.** The driver reports actual
milliwatts through `IOCTL_BATTERY_QUERY_STATUS`, which is hardware-measured
whole-system draw. CPU package watts (RAPL) would need a signed kernel driver on
Windows and would be strictly worse — CPU load is used only as a context
feature, via one `GetSystemTimes` call.

**Two tracks, because the reported rate lies.** Measured on the development
machine, the driver's reported rate overstated true energy flow by ~50% in the
minutes after unplugging. So the responsive figure (30 s EWMA of reported rate)
drives the display, while a least-squares slope of the capacity gauge over 15
minutes — the energy that actually left the pack — drives the medium-horizon
prediction.

**Discharge blends three horizons.** Expected draw at horizon `h` hands off
smoothly from what the machine is doing now, through the last hour, to how it
normally behaves in this context (activity × CPU-load bucket). Because draw
varies with `h`, time-to-empty is solved by forward integration, not division.
The usable floor is learned from where this machine actually hits critical.

**Charging integrates a learned curve.** Lithium cells charge constant-current
then constant-voltage, so the rate collapses near full — on this hardware the
taper starts around 60%, not the textbook 80%. The app learns a per-device
rate-vs-SoC curve, stored as a normalised shape plus a separately tracked peak,
so swapping to a weaker charger rescales the curve instead of corrupting it.
The peak is tracked as an 80th percentile rather than a mean: charge data
includes trickle and near-full periods, and a mean over them read 17 W against a
real 27 W charger.

**One milestone at a time while charging.** Below 80% the panel shows time to
80%; past it, time to full. Never both — charging slows sharply at that point,
so the two numbers describe different regimes and showing them together invites
reading the wrong one.

**Charge limits are detected, not guessed at.** If capacity sits still on AC
below 95%, the app reports "held at N% — charge limit" rather than an ETA that
will never arrive. Surface Battery Limit and Windows Smart Charging both do this.

**It grades itself.** Every prediction is logged with its target; when the target
is reached the actual elapsed time is recorded. A rolling median of
actual/predicted becomes a correction factor, and its spread becomes the
confidence shown on hover. Checkpoints are near-term (a 15% SoC move) so they
actually resolve — time-to-empty would otherwise almost never be gradeable,
since the machine dies before the outcome can be recorded.

**It is accurate on day one.** On first run it parses
`powercfg /batteryreport /xml` in the background: 14 days of real charge sessions
and discharge segments bootstrap the curve and the priors. A wide charge session
constrains the *integral*, not the shape — attributing its average rate to every
SoC bucket it spans would flatten the very taper the curve exists to capture.

## Measured accuracy

Replay a recorded trace to measure end-to-end error against a naive
`remaining ÷ current rate` predictor:

```sh
cargo run --release -p battery-core --example replay -- trace.csv [--seed report.xml]
```

On a 39-minute real charge session from this machine:

| | model | naive baseline |
|---|---|---|
| median abs error | **13.2%** | 18.9% |
| p90 abs error | 36.5% | — |

Discharge is not yet validated on a long real session — the available trace held
only 5 minutes off AC, which is too short for the 15-minute capacity slope to
engage. The unit tests cover the discharge maths analytically; a real multi-hour
recording would be the honest confirmation.

## Drawing

The logo, the tray glyphs and the embedded `.ico` all come from one place:
`battery_core::glyph`, which rasterises shapes by supersampled coverage testing.
The build script renders the icon from that same code, so the artwork cannot
drift from what the app draws at runtime. Panel and settings windows composite
into a single DIB in software — which is what gives the graph a properly
anti-aliased, smoothed curve — with GDI used only for text.

The graph runs edge to edge and scrolls continuously: the x axis is a function
of the current time, so a repaint twice a second is enough to make it drift
without any animation state. It advances about a pixel every ten seconds. The
oldest part fades out at the left edge rather than being clipped mid-stroke,
and throughput is coloured by direction — into the battery green, out of it red,
split at the zero line.

When every reading in the window flows the same way — nothing has been plugged
in for an hour, say — the plot is handed entirely to that direction and the
zero line is dropped, rather than holding half the height empty for a sign that
never appears. Both that and the zero line can be turned off, as can the graph
itself.

It draws only the span it actually has samples for. Holding the earliest reading
across the rest of the window would render hours the app was not running as a
flat line, which reads as real history rather than the absence of it. When there
is not enough history to plot, the graph is dropped and the window shrinks
instead of showing an empty band.

## Layout

```
crates/battery-core   pure Rust: filters, curve, priors, estimator, scoring,
                      persistence, report seeding, alerts, glyphs. No Win32, 102 tests.
crates/battery-win    battery IOCTLs, CPU load
crates/battery-tray   tray icon, popup panel, settings window, message loop
```

## Cost

Measured while running: **~2.5 MB private working set**, ~0.05 s CPU per minute,
a 530 KB executable, no runtime dependency. Sampling blocks in the kernel via
`BATTERY_WAIT_STATUS` and wakes on real change or a 5-second timeout, with a
fallback to timed polling if a driver ignores the wait. Tray icons are cached by
appearance, so a sample that does not change the displayed value draws nothing.

Sampling runs at 1 s while the panel is open and 5 s when it is not, since a
fresher reading is only worth anything on screen.

State lives in `%LOCALAPPDATA%\BatteryTray\`:

- `model.json` — learned curve, priors, corrections. Discarded if written by a
  different version, since stale beliefs are invisible once stored as numbers.
- `settings.json` — user preferences. Repaired field-by-field rather than
  discarded, and tolerant of a byte-order mark, so hand editing is safe.
- `history.bin` — 24 h of samples for the graph.
- `panic.log` — written if the app ever dies unexpectedly. A windowed process
  has no console, so without this a panic in a window procedure disappears and
  Windows reports only a fault inside whichever system DLL made the call.

"Reset learned data" clears the model and re-seeds; it leaves settings alone.
