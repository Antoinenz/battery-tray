# Changelog

## v0.1.1

Visual fixes, all in the panel.

- **The throughput graph reads the right way up when it shows drain alone.**
  Drain is negative, so when the plot was handed entirely to it the curve hung
  from the top edge — which only makes sense against a zero line, and a plot
  given over to one direction has dropped its zero line. It is flipped now:
  more draw, taller. It goes back below the line as soon as charge reappears.
- **The graph runs out to the right edge.** History is written every half
  minute while the panel repaints several times a second, so the newest
  columns were always empty and the curve stopped a few pixels short. It now
  carries the live reading through to the present — the same value the header
  shows, not an extrapolation.
- **The callout tail is outlined.** It was filled but never stroked, so the
  panel's border stopped dead where the two windows meet.
- **The wash under the curve is fainter.** It was competing with the line for
  attention; the line is the reading, and the wash only says which side of it
  is filled.
- **The header is tighter and the panel is 10px shorter.** One space between
  the wattage and its direction rather than two, less air above the throughput
  row, and the gap under it closed by shortening the window rather than moving
  the space somewhere else.

## v0.1.0

First release. A Windows tray app that predicts battery time to empty, to 80%
while charging, and to full, from the watts actually entering or leaving the
battery and from what it has learned about this machine.
