# Progressive loading: big catalogues and big versions show at once

## Why

Opening a project downloads its whole state before anything shows: STIX (107k events) is
24 MB compressed and several seconds of nothing but a progress note. The history has the same
shape: a version's changes come in one answer, so a STIX refresh (thousands of changed events)
shows "Loading the changes…" until the server has rebuilt the project twice and diffed it.
The user (2026-11-11): progressive loading rather than pagination, for the catalogue tables
and for the history/diff view.

## What it is (decided by the user)

- **Catalogue page:** the first rows show within a second, read-only, while the project keeps
  downloading; more rows stream in; when the whole project has arrived the page switches to the
  live copy without losing the visitor's place.
- **History:** a version's change counts and first changes show at once; the rest arrives as
  the visitor scrolls.

## Goals

- First rows of a catalogue on screen in about a second, whatever its size.
- A version's summary (counts per catalogue) and first changes on screen in about a second.
- Nothing changes for small projects: when the whole project arrives before the first page,
  no swap happens.
- The switch to the live copy keeps the scroll position, the selected row and the sort.
- Python and SciQLop keep working unchanged (the history route stays backward compatible).

## Non-goals

- Editing, search, statistics or live updates before the whole project has arrived (greyed
  out with a short reason, then enabled at the switch).
- Partial CRDT sync: the live copy still needs the whole project.
- Changing how the table draws (it already draws only the visible rows).

## Anti-goals

- Two tables flashing, or a visible jump, at the switch.
- The preview showing different data than the live copy will (same order, same columns).
- The server rebuilding a project once per page of the same version.

## Shape (decided parts)

### Catalogue preview

- While the project downloads, the open catalogue's events come from the existing
  `GET /api/rooms/{project}/catalogues/{catalogue}/events` (cursor pages, sorted by start):
  first page of 1000 at once, the next pages one after another in the background (up to
  10000 each), appended as they come.
- The table shows the preview rows with the same columns and order rules as the live table;
  the header says "Showing {n} of {total} events; the rest is loading…" with the download
  progress already shown by the loading note.
- "All events" (the project-wide view) gets the same preview from the project's catalogues'
  pages (builder's call how), or keeps today's loading note if that is not cheap — the builder
  says which.
- At the switch, the live table takes the preview's scroll offset, selection (by event uuid)
  and sort.

### History pages

- `GET /api/rooms/{project}/history/{version}` gains optional `limit` and `cursor`. With a
  `limit` the answer holds `counts` (per catalogue: added, changed, removed, and totals) and
  the first `limit` event changes, plus `next`; without one it answers as today (all changes)
  plus `counts`, so Python keeps working.
- The server keeps the last few computed diffs in memory (keyed by project, version range and
  catalogue; a small LRU with a time limit), so the following pages and a reopened version do
  not rebuild the project.
- The web history asks for 200 at a time and loads the next page as the visitor nears the end
  of the list.

## Calls made after the builder check (mine — check them)

- **Counts**: `counts` = `{total: {added, changed, removed}, catalogues: [{catalogue, name,
  added, changed, removed}]}` counting event changes; an event in several catalogues counts in
  each catalogue's row and once in `total`. With `?catalogue=`, only that catalogue's row and a
  total equal to it. Catalogue-level changes (renamed, created, deleted) are few: they come
  complete in every answer, not paged.
- **Order of changes**: by the event's start (after the change, else before), then uuid; the
  cursor is that pair, as for the events route.
- **Preview order**: by start, as the events route gives; sorting is off in the preview (the
  column headers say "sorting once loaded"); at the switch the live table applies its sort.
- **Preview cost**: each page of the events route scans the catalogue once; for STIX about a
  dozen scans during the download. Accepted, with a `simplify:` note naming a per-room sorted
  cache as the upgrade.
- **"All events"** keeps today's loading note (no project-wide events route).
- **Edits during the download**: the preview may be stale; the live copy wins at the switch.
- **Diff cache key** includes the version's end record, so the open, still-growing version
  misses naturally; entries expire after a few minutes.

## Left to the builder

- The LRU size and lifetime (state the numbers in a `simplify:` note).
- The order of changes in pages (must be stable across pages).
- How the preview and the live table share code (one table fed by either source preferred).
