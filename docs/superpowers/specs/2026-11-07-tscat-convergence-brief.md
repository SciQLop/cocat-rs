# Brief: cocat-rs replaces tscat (2026-11-07)

Written by the tscat session after a design discussion with the user. The decisions below
are the user's. The tasks are ordered. Brainstorm and plan as usual. Ask the user only about
choices this brief leaves open.

Vision doc (context only): https://claude.ai/artifact/7x3YCfVgBhg9oJFUnHPQd6

## Decisions

- cocat-rs becomes the one catalogue engine. tscat is frozen at 0.5.x.
- **No backward compatibility for cocat.** It has no users yet. The room layout may change
  freely, Python cocat included.
- **No tscat compatibility layer.** Migrate a local tscat database into a local room once.
  SciQLop drops its tscat plugin and tscat_gui and uses the cocat provider with a local room
  that never connects. (The SciQLop side is not part of this brief.)
- Zenodo push: later, not now.
- Smart catalogues (rule-based) are a major goal. They come after the tasks below.

## Tasks, in order

1. **Room file compaction.** When a room opens and its file is much larger than its state,
   rewrite it as one state update: temp file, fsync, rename. See the `simplify:` note in
   `cocat-client/src/room.rs` (~line 455). Add an open-time benchmark on `bench100k`
   (today: ~4.9 s open, 36 MB file, via `cocat_rs.Room.open`).

   **Skipped (user decision, 2026-11-07), after measuring.** The released 0.1.3 wheel opens
   bench100k in 0.74 s (the 4.9 s was a debug build). bench100k's file equals its state
   (37.80 MB both: 100 large updates), and the production rooms are 6–21 % above theirs
   (alexis.jeandet 3.27 → 3.08 MB, Solitary Waves 521 → 414 KB). Opening from one compacted
   update is no faster than replaying the file (0.75 s vs 0.70 s): building the document is
   the cost, not the record count. The `simplify:` note stays; revisit after data model v2.
2. **Time algebra in cocat-core**, pure functions over event lists, exposed in `cocat_rs`.
   Use bedtools names: union/merge, intersect, subtract, closest (gap as output), window
   (within Δt), shift, tile. Half-open intervals; define and test the edge cases.
3. **tscat predicate evaluator in Rust.** Read tscat's predicate JSON (`tscat/filtering.py`,
   `to_dict`/`from_dict`: Comparison, Match, Not, Has, All, Any, In, UUID, InCatalogue) and
   evaluate it against a room. Evaluation only; storing rules in rooms comes with smart
   catalogues.
4. **tscat → room converter**, in Python. Read through tscat itself (it handles its own
   alembic migrations, including pickle → JSON predicates), write through `cocat_rs`.
   Keep uuids. Rating → confidence = rating / 10. Removed tscat items → status `deleted`.
   Dynamic catalogues: store the predicate JSON in a catalogue attribute `tscat_predicate`.
   Also: tscat-format JSON import/export and `existing_tags`.
5. **Data model v2** (spec below). Draft it as a spec for the user to review before
   implementing. Measure bytes per event before and after on `bench100k`; target: a
   hand-made event grows < 10 %.

TFCat import/export is unblocked too (spec: doi:10.25935/6068-8528), lower priority.

## Data model v2 spec (agreed)

Principle: an event stores a field only when it differs from the default. Anything that
is the same for a whole run lives in a shared record, referenced by id. In Yjs every map key
is a separate item that repeats the key string, so fewer keys per event matters most. Avoid
nested maps per event.

**Event, always:** `uuid`, `start`, `stop` (consider integer ns instead of text).

**Event, only when set:**

| Field | Meaning / source of the idea |
| --- | --- |
| `author`, `created` | Hand-made events only; model events take these from their origin |
| `origin` | Id of an origin record; absent = made by hand |
| `status` | `preliminary`, `confirmed`, `reviewed`, `final`, `rejected` (QuakeML evaluationStatus) plus `deleted` (= trash). Absent = `confirmed` |
| `confidence` | 0–1. Replaces tscat's rating |
| `start_err`, `stop_err` | Symmetric ± in seconds (one number per edge) |
| `shape` | Optional time–frequency polygon (TFCat geometry). If set, start/stop are computed from it on write |
| `source_id` | Id of the event in an imported list |
| `tags`, `products`, `attributes` | As today |

Dropped as redundant: manual/automatic mode (follows from `origin`), version and supersedes
(the CRDT history covers them), per-event data-used (in origin), units per value
(in the registry), four-number asymmetric uncertainties.

`rejected` is a scientific judgement: it stays visible as a negative example for ML.
`deleted` is a mistake: it goes to the trash.

**Root maps besides events and catalogues:**

- `origins`: kind (`rule` | `detector` | `model` | `import`), name, version, author, created,
  data products used with version or retrieval date, parameters, source DOI for imports.
  Names follow W3C PROV ideas (generated-by, used, derived-from).
- `comments`: keyed by event uuid, so rare long text stays out of event reads.
- `attributes` (registry): key → type, plus optional unit and description. **One definition
  per key per room**, not per catalogue, because an event can sit in several catalogues and
  a smart catalogue mixes events from many. A catalogue only lists which keys it shows as
  columns.

**Registry rules. The user's constraint: never make creating an event by hand painful.**

- A new key is registered silently on first use, with its type guessed from the value
  (number, text, bool, date). No dialog, no required unit.
- Unit and description are optional, editable later.
- A value whose type doesn't match is saved anyway, with a warning. CRDT merges must never
  drop an edit because of the registry.
- No UCD and no unit conversion for now. Same quantity in another unit = another key.

**Catalogue level, later:** DataCite-style metadata (description, licence, ORCID authors,
related ids) for DOI snapshots.

## Working rules (from the user's global instructions)

- TDD: a failing test first, for features and bugs alike.
- One build or test run at a time, in the foreground. Never run builds in parallel or in the
  background. This applies to any sub-agent too.
- Commit on cocat-rs main after the full suite passes. Never push.
- Prefer established crates over hand-rolled code.
