# Drawing

Notes on how the panel is rendered, and why it is rendered that way.

## One source for the artwork

The logo, the tray glyphs and the embedded `.ico` all come from
`battery_core::glyph`, which rasterises shapes by supersampled coverage testing
— at tray sizes that is both smaller and sharper than scaling down a vector
path. The build script renders the icon from the same code, so the artwork
cannot drift from what the app draws at runtime.

Panel and settings windows composite into a single DIB in software, which is
what gives the graph a properly anti-aliased curve. GDI is used only for text.

## The graph

It runs edge to edge and scrolls continuously: the x axis is a function of the
current time, so a repaint a few times a second is enough to make it drift
without any animation state. It advances about a pixel every ten seconds.

Throughput is coloured by direction — into the battery green, out of it red,
split at the zero line.

**When everything flows one way** — nothing has been plugged in for an hour,
say — the plot is handed entirely to that direction and the zero line is
dropped, rather than holding half the height empty for a sign that never
appears. Drain is negative, so on its own it would hang from the top edge; with
no zero line left to give the sign meaning it is flipped instead, and reads the
way any single-quantity chart does: more draw, taller. It goes back below the
line the moment charge reappears.

**The wash under the curve** fades with distance from it, and is deliberately
faint — the line is the reading, and the wash only says which side of it is
filled. With a zero line to land on it stays tight and keeps a floor, so the
area reads as filled all the way down; without one there is nothing to land on,
so it reaches further and dissolves rather than stopping at an edge.

**The left-hand fade is anchored to the start of the line**, not the edge of
the plot. Anchored to the plot, a short history would get no fade at all,
because the ramp would be over before the data began.

**It draws only the span it has samples for.** Holding the earliest reading
across the rest of the window would render hours the app was not running as a
flat line, which reads as real history rather than the absence of it. When
there is not enough history to plot, the graph is dropped and the window
shrinks instead of showing an empty band.

**It reaches the right edge.** History is only written every half minute while
the panel repaints several times a second, so the newest columns would
otherwise always be empty. The chart carries the live reading forward to the
present — the same value the header shows, not an extrapolation — but only once
there is one, since drawing the startup placeholder yanks the curve to zero.

## The moving percentage

The charge gauge only reports whole 10 mWh steps — about 0.026% on a 39 Wh pack
— so a two-decimal readout taken straight from it lurches.

Dead reckoning alone is not enough either: the rate it integrates comes from a
filter that steps whenever a sample lands, so the figure changes pace visibly,
and it stalls whenever drift hits the step bound.

`SocDisplay` carries its own value instead, moving it at a velocity that itself
eases toward the measured one, so the counter accelerates and decelerates with
load rather than switching between speeds. That value is pulled gently back
toward the reading, which keeps it honest without the correction ever showing
as a jump, is bounded to within one and a half gauge steps of it, and is never
allowed to move against the direction the battery is going.

It is tested against a simulated quantising gauge and holds every increment
within 15% of the mean.

## The tray icon

Percentages in the tray stay whole even with decimals enabled. Legibility at
20 px is set by how many glyphs must fit, so time is stacked as two lines —
hours over minutes, like a clock — rather than squeezed into `2:45`, and the
finer reading lives in the panel where there is room for it.

Icons are cached by appearance, so a sample that does not change the displayed
value draws nothing at all.

## The settings window is not custom-drawn

The panel has to be — it is a chart. Settings are a form, and real `BUTTON`,
`STATIC`, `COMBOBOX` and `SysTabControl32` controls inherit theming, keyboard
navigation, focus rings, high-contrast modes and screen-reader support for
free. A Common Controls v6 manifest is what makes them render themed rather
than in the Windows 95 style.
