# How the prediction works

The detail behind the summary in the [README](../README.md).

## Power comes from the battery, not from guesswork

The driver reports actual milliwatts through `IOCTL_BATTERY_QUERY_STATUS`, which
is hardware-measured whole-system draw. CPU package watts (RAPL) would need a
signed kernel driver on Windows and would be strictly worse — CPU load is used
only as a context feature, via one `GetSystemTimes` call.

## Two tracks, because the reported rate lies

Measured on the development machine, the driver's reported rate overstated true
energy flow by ~50% in the minutes after unplugging. So the responsive figure —
a 30 s EWMA of the reported rate — drives the display, while a least-squares
slope of the capacity gauge over 15 minutes, the energy that actually left the
pack, drives the medium-horizon prediction.

## Discharge blends three horizons

Expected draw at horizon `h` hands off smoothly from what the machine is doing
now, through the last hour, to how it normally behaves in this context
(activity × CPU-load bucket). Because draw varies with `h`, time-to-empty is
solved by forward integration rather than division. The usable floor — the
charge at which the machine actually stops, which is not 0% — is learned from
where this machine hits critical.

## Charging integrates a learned curve

Lithium cells charge constant-current then constant-voltage, so the rate
collapses as the cell fills. The textbook figure is 80%; on the development
machine the taper starts nearer 60%, which is exactly why the curve is learned
per device rather than assumed.

The curve is stored as **a normalised shape plus a separately tracked peak**:

- The **shape** is 20 buckets of 5% SoC each, holding the rate at that charge
  level as a fraction of the peak. It says *how the taper falls away*.
- The **peak** is the charger's actual output in watts. It says *how fast*.

Splitting them is what makes a charger swap safe. Plug in a weaker charger and
only the peak changes; the shape — the physics of the cell — is untouched, so
the curve rescales instead of being corrupted by readings from two different
chargers averaged together.

The peak is tracked as an **80th percentile**, not a mean. Charge data is full
of trickle and near-full periods, and a mean over them read 17 W against a real
27 W charger. The percentile is estimated online by a Robbins-Monro update
rather than by keeping the samples.

Time to a target is then forward integration: step through SoC in 0.5%
increments, look up the rate for each step from shape × peak, and accumulate.

**One milestone at a time.** Below 80% the panel shows time to 80%; past it,
time to full. Never both — charging slows sharply at that point, so the two
numbers describe different regimes and showing them together invites reading
the wrong one.

## Charge limits are detected, not guessed at

If capacity sits still on AC below 95%, the app reports "held at N% — charge
limit" rather than an ETA that will never arrive. Surface Battery Limit and
Windows Smart Charging both do this.

## It grades itself

Every prediction is logged with its target; when the target is reached, the
actual elapsed time is recorded. A rolling median of actual/predicted becomes a
correction factor, and its spread becomes the confidence shown on hover.

Checkpoints are near-term — a 15% SoC move — so they actually resolve.
Time-to-empty would otherwise almost never be gradeable, since the machine dies
before the outcome can be recorded.

## It is accurate on day one

On first run the app parses `powercfg /batteryreport /xml` in the background:
14 days of real charge sessions and discharge segments bootstrap the curve and
the priors, so the first prediction is not a guess from a nameplate figure.

A wide charge session constrains the **integral**, not the shape. Attributing
its average rate to every SoC bucket it spans would flatten the very taper the
curve exists to capture, so wide sessions only calibrate the peak.

## Measured accuracy

Replay a recorded trace to measure end-to-end error against a naive
`remaining ÷ current rate` predictor:

```sh
cargo run --release -p battery-core --example replay -- trace.csv [--seed report.xml]
```

On a 39-minute real charge session from the development machine:

| | model | naive baseline |
|---|---|---|
| median abs error | **13.2%** | 18.9% |
| p90 abs error | 36.5% | — |

**Discharge is not yet validated on a long real session.** The available trace
held only 5 minutes off AC, which is too short for the 15-minute capacity slope
to engage. The unit tests cover the discharge maths analytically; a real
multi-hour recording would be the honest confirmation.
