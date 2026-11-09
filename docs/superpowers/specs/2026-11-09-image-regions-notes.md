# Notes: shapes in images (not planned yet)

Status: discussion only, saved for later. Nothing here is built. What is built is in
`2026-11-09-tfcat-design.md`: time-series shapes, with a second axis the catalogue describes
(frequency, energy, ...).

## The idea

Catalogue features seen in images, such as sunspots, with the same events and catalogues as
time intervals.

## What we agreed on

- The images are frames of movies, so an image region is an event at one instant:
  **start = stop = the frame's time**, with a 2D outline.
- The outline's axes are both spatial (for example x/y in arcseconds on the Sun,
  helioprojective, or in pixels). The catalogue would describe both axes, the way it
  describes the second axis of a time-series shape.
- The rule "a shape sets start/stop from its time span" only applies when one axis is time.
  For a spatial outline, the caller sets start = stop = frame time.
- The movie the outline belongs to goes in the event's existing `products` field.
- A feature seen on many frames, recommended form: **one event per frame**, sharing an
  attribute such as `track = "AR 13664"`. Filtering by the track gives the sequence. The
  alternative (one event holding an outline per frame) is tidier but a new, more complex
  kind of shape.

## Prior art to read when this starts

- The Heliophysics Events Knowledgebase (HEK) stores feature outlines as polygons in
  helioprojective coordinates.
- IVOA STC-S for regions; TFCat for the time-series case.

## When to pick it up

When there is a concrete image or movie dataset to catalogue.
