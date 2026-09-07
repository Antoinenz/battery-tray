# Battery Time Remaining — Design

**Date:** 2026-09-07
**Status:** Approved, implementing

## Goal

A Windows tray app that predicts, accurately: time to empty, time to 80% while
charging, and time to full. Display live watts entering or leaving the battery.
Priorities, in order: **accuracy**, **low resource use**, **polish**.

## Measured hardware facts (this machine, Surface, 12th-gen i7-1265U)

| Fact | Value | Consequence |
|---|---|---|
| Capacity units | mWh / mW (`BATTERY_CAPACITY_RELATIVE` clear) | Real watts available |
| Full charge capacity | 38,820 mWh vs 47,700 design (81%) | Battery aged; 533 cycles |
| FCC day-to-day drift | 38,323 → 39,655 mWh | FCC must be smoothed, not trusted as constant |
| Sensor update period | ~2 s | 5 s sampling ceiling is ample |
| Reported rate noise | ±1.8% | Light filtering suffices |
| Charge rate at 15–50% SoC | ~26.9 W (CC plateau) | — |
| Charge rate at 65–70% | ~6–21 W | Taper starts ~60%, earlier than textbook 80% |
| Charge rate at 95–100% | ~3.4 W | Linear estimate wrong by ~8× near full |
| Discharge, Active | 16.5 W median (p25 14.0, p75 19.4) | Tight → confident predictions |
| Discharge, ConnectedStandby | 2.2 W median | 7× lower than Active |
| Discharge, Suspend | 0.8 W median | — |

The charge taper and the 7× activity split are the two facts that dominate accuracy.

## Non-goal: CPU package watts

Reading RAPL/MSR on Windows requires a signed kernel driver. It is also
unnecessary: the battery's own `Rate` is hardware-measured *total system* draw,
CPU included, and is strictly better ground truth. CPU utilisation is used only
as a cheap context feature (`GetSystemTimes`, one syscall).

## Architecture

```
crates/battery-core   pure Rust, no Win32, unit-tested — filters, curve, priors,
                      estimator, scoring, persistence, report seeding
crates/battery-win    native battery IOCTLs, CPU load, power notifications
crates/battery-tray   tray icon, popup panel, message loop
```

### Data acquisition

`SetupDiGetClassDevs(GUID_DEVICE_BATTERY)` → `CreateFile` → battery IOCTLs
(`QUERY_TAG`, `QUERY_INFORMATION`, `QUERY_STATUS`). No WMI: every WMI query
spawns `WmiPrvSE.exe` and costs tens of MB.

Sampling is event-driven. `IOCTL_BATTERY_QUERY_STATUS` is issued with a
`BATTERY_WAIT_STATUS` block carrying the *current* power state and a capacity
band; the kernel blocks the sampler thread and returns only on a real change or
a 5 s timeout. Verified: returns in 1.1 s on an actual change.

Guard: if waits return in <200 ms repeatedly (a driver that ignores the wait),
fall back to timed polling.

### Two-track rate estimation

- **Track A, responsive** — reported `Rate`, EWMA τ=30 s. Drives the watt readout.
- **Track B, truthful** — least-squares slope of `Capacity` over a 15 min window.
  Immune to reported-rate bias; 10 mWh quantisation washes out over the window.
- **Reconciliation** — calibration factor `k = B/A`, EWMA, clamped [0.8, 1.25].
  Display stays snappy; predictions consume calibrated energy flow.

### Prediction

**Discharging.** Usable energy = `capacity − reserve`, reserve learned from the
capacity at which this machine actually hit critical. Future draw is blended
live→prior as the horizon grows:

```
P(h) = w(h)·P_live + (1−w(h))·P_prior,   w(h) = exp(−h/τ),  τ = 1200 s
```

Time-to-empty is solved by forward integration in 60 s steps, because P varies
with h. `P_prior` comes from the context bucket (activity × CPU load band).

**Charging.** Learned rate-vs-SoC curve: 20 buckets of 5% SoC holding a
*normalised shape* (fraction of peak) plus a separately tracked `peak_mw`, so
plugging into a weaker charger rescales the whole curve instead of corrupting
it. Both ETAs integrate energy needed against learned rate per bucket. Seeded
from the empirical shape measured above; refined per device over time.

**Charge-limit / plateau detection.** If capacity is flat while on AC below 95%
SoC, report "held at N%" rather than an ETA that never arrives. (Surface Battery
Limit and Windows Smart Charging both do this.)

### Self-evaluation

Every prediction is logged with its target; when the target is reached, the
actual elapsed time is recorded. A rolling median of `actual/predicted` per
prediction kind becomes a correction factor (clamped [0.6, 1.6]), and the rolling
relative error drives the displayed confidence band. The app measurably improves
against its own history rather than merely appearing to.

### Cold start: seeding from Windows' own records

`powercfg /batteryreport /xml` exposes 14 days of state transitions with mWh
capacity, plus `Drain` sessions. On first run (and monthly), a background thread
parses it and bootstraps the charge curve and discharge priors. The app is
accurate on day one instead of after a week of learning.

### Storage

`%LOCALAPPDATA%\BatteryTray\`
- `model.json` — curve, priors, reserve, corrections. Atomic write (tmp+rename).
- `history.bin` — 24 h of samples at 30 s resolution (~35 KB), for the sparkline.

Deliberately not SQLite: the query patterns are trivial, and avoiding it removes
a C toolchain dependency and ~1.5 MB of binary. (Deviation from the original
sketch, made for the stated lightweight priority.)

### UI

- **Icon** — GDI-drawn battery glyph, 4× supersampled and downsampled for clean
  edges, DPI-sized via `SM_CXSMICON`, cached per (level, state, theme). Follows
  the system light/dark setting.
- **Tooltip** — SoC, signed watts, all live estimates.
- **Panel** — left-click. Win11 look via DWM rounded corners + acrylic backdrop
  + immersive dark mode. Sparkline click-toggles between 60 min watts and 24 h
  SoC. Dismisses on focus loss.
- **Menu** — right-click: start with Windows, reset learned data, diagnostics, quit.

### Budget

< 5 MB working set, < 0.1% CPU idle, ~1 MB exe, no runtime dependency.
Single sampler thread + message loop; no async runtime.

### Testing

`battery-core` is pure and takes recorded traces as input. Unit tests cover
filters, curve integration, plateau detection, seeding, and scoring. Accuracy is
asserted against real traces recorded from this machine, not merely asserted.
