# BatteryTray

A small Windows tray app that tells you how long your battery will actually
last. It reads the watts genuinely entering or leaving the battery, and learns
how *your* machine behaves — so the estimate gets better the longer you run it,
instead of being a nameplate figure divided by a guess.

<p align="center">
  <img src="docs/screenshots/tray-time-remaining.png" alt="The panel open above the taskbar, showing 98.49%, 7.8 W out and 3h 22m remaining" width="467">
</p>

Left-click the tray icon for the panel. That is the whole interface.

## Features

- **Time to empty, to 80%, and to full** — one number at a time, whichever is
  the one that matters right now.
- **Real watts in and out**, measured by the battery itself rather than
  inferred from CPU usage.
- **A live graph** of charge level or power throughput, scrolling continuously,
  coloured by direction — green in, red out.
- **It says how sure it is.** Hover the time row for the uncertainty range and
  a confidence word, both earned from grading its own past predictions.
- **Battery health** — full-charge versus design capacity, cycle count,
  chemistry.
- **Five tray icon styles** — battery preview, percentage, wattage, time
  remaining, or just the logo.
- **Optional alerts** at low and critical charge, at 80%, and at full.
- **Light and dark**, following Windows or pinned either way.
- **Tiny.** ~2.5 MB of RAM, a rounding error of CPU, one ~530 KB exe, no
  installer and no runtime to install.

<p align="center">
  <img src="docs/screenshots/confidence.png" alt="Hovering the time row shows a range of 2h 10m to 6h 32m, and 'Still learning'" width="463">
</p>

## Install

Download `battery-tray.exe` from the
[latest release](https://github.com/Antoinenz/battery-tray/releases/latest) and
run it. There is nothing to install and nothing to configure — it starts
predicting immediately, using Windows' own battery history for the first few
hours until it has gathered its own.

Turn on **Start with Windows** in Settings if you want it back after a reboot.

To uninstall: quit it and delete the exe. Its settings and learned data live in
`%LOCALAPPDATA%\BatteryTray`, which you can delete too.

## Using it

| | |
|---|---|
| **Left-click** the tray icon | Open the panel — click again to close |
| **Right-click** it | Settings and Quit |
| **Hover** the time row | Uncertainty range and confidence |
| **Click** the graph | Switch between charge level and throughput |

The panel closes when you click anything else, unless you pin it in Settings —
pinned, it stays until dismissed and can be dragged anywhere on screen.

<p align="center">
  <img src="docs/screenshots/settings-display.png" alt="The Display tab of the settings window" width="420">
</p>

## How it learns your battery

Most battery estimates are `charge remaining ÷ current draw`. That is wrong in
both directions, and this app avoids it in two different ways.

**Discharging**, the draw you are pulling right now is a poor guide to the next
three hours, because you will not keep doing what you are doing. So expected
draw is blended across three horizons — this instant, the last hour, and how
this machine normally behaves in this context — and the time remaining is
solved by integrating that forward, not by dividing.

**Charging** is the more interesting half, because charging is not linear.

A lithium cell charges at constant current until it approaches full, then
switches to constant voltage, and the power going in collapses. The last 20%
can take as long as the first 60%. Any predictor that assumes a steady rate
will promise you a full battery long before you get one.

So the app learns the shape of your charger and your cell, stored as two
separate things:

- **The shape** — 20 buckets of 5% charge each, holding the rate at that level
  as a fraction of the peak. This is *how your taper falls away*. The textbook
  says it begins at 80%; on the machine this was built on it begins nearer 60%,
  which is exactly why it is measured rather than assumed.
- **The peak** — what your charger actually delivers, in watts. This is *how
  fast*.

Keeping them apart is what makes swapping chargers safe. Plug in a weaker one
and only the peak moves; the shape, which is the physics of the cell, stays
put. Averaging the two chargers together into one curve would corrupt both.

The peak is tracked as an **80th percentile** rather than an average, because
charge data is full of trickle and nearly-full periods — a mean over them read
17 W against a real 27 W charger.

Time-to-full is then walking that curve: step through the charge levels between
here and there, look up the rate for each, and add up the minutes.

Finally, **it marks its own work.** Every prediction is filed with its target;
when the target arrives, the real elapsed time is recorded against it. The
running median of actual-versus-predicted becomes a correction applied to
future estimates, and how widely those grades scatter becomes the confidence
you see on hover. That is why a fresh install says "Still learning" and an old
one does not.

The Learning tab in Settings shows all of this as it accumulates.

For the full detail — the two-track rate estimation, the charge-limit
detection, and measured accuracy against a naive baseline — see
[docs/prediction.md](docs/prediction.md).

## For developers

```sh
cargo build --release      # target/release/battery-tray.exe
cargo test --workspace
```

A stable Rust toolchain is all you need. Three crates: `battery-core` is pure
Rust and holds the estimator, the curve, persistence and the tests;
`battery-win` wraps the battery IOCTLs; `battery-tray` is the Win32 layer —
tray icon, panel, settings, message loop. No WMI, no COM, no runtime.

- [docs/development.md](docs/development.md) — layout, debugging, the replay
  harness, files on disk, cutting a release
- [docs/prediction.md](docs/prediction.md) — how the estimates are actually made
- [docs/drawing.md](docs/drawing.md) — how the panel and the graph are rendered

## More screenshots

<details>
<summary>Click to expand</summary>

**Fully charged, with each kind of graph**

<img src="docs/screenshots/charged-throughput-graph.png" alt="Panel showing 98.57%, fully charged, with the throughput graph" width="460">

<img src="docs/screenshots/charged-level-graph.png" alt="Panel showing 98.66%, fully charged, with the battery-level graph" width="460">

**Discharging**

<img src="docs/screenshots/tray-throughput-graph.png" alt="Panel showing 91.99%, 18.6 W out and 2h 21m, the graph turning red as draw begins" width="403">

<img src="docs/screenshots/panel-close.png" alt="Close view of the panel showing 25.7 W out and 3h 49m" width="403">

**Battery health**

<img src="docs/screenshots/settings-battery.png" alt="The Battery tab, showing 39.4 Wh of 47.7 Wh design capacity, 83% health, 535 cycles" width="420">

**What it has learned**

<img src="docs/screenshots/settings-learning.png" alt="The Learning tab, showing a fully learned charge curve, 32.1 W charger and 33 graded predictions" width="420">

</details>

## Licence

MIT. See [LICENSE](LICENSE).
