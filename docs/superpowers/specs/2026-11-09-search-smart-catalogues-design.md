# Search, and smart catalogues saved from it (Papers style)

## Why

A smart catalogue is edited today as raw rule JSON. Papers (ReadCube) shows a better model:
search first, with a field syntax in the search bar; the list filters live; any search can be
saved as a Smart List, whose query is its rule; a Query Builder writes the same query for
people who don't know the syntax; presets give common lists in one click.
(Papers help: "Advanced Searching (In Library)", "Introduction to Smart Lists",
"Introduction to the Query Builder".)

## The query language

One text form, parsed in `cocat-core` (`query.rs`) into the existing rule `Predicate`, and
printed back from it. Python, the web app and SciQLop share it. Parse with a
well-established parser crate (e.g. `winnow` or `chumsky`), not by hand.

- **Words.** A bare word or "quoted phrase" matches any text of the event: tags, status,
  author, source id, origin, attribute values (case-insensitive, contains). This is today's
  filter box behaviour, kept.
- **Fields.** `field:value`, `field:"two words"`. Text fields match contains,
  case-insensitive; `field:/regex/` is a regex match.
- **Comparisons.** `field:>v`, `>=`, `<`, `<=`; ranges `field:[a TO b]`, with `*` for an open
  end.
- **Existence.** `_exists_:field`; `NOT(_exists_:field)` for missing.
- **Logic.** Space = AND; `AND`, `OR`, `NOT` (upper case); parentheses; precedence NOT, AND,
  OR, as in Papers.
- **Field names.** `start`, `stop`, `duration`, `author`, `created`, `origin`, `status`,
  `confidence`, `start_err`, `stop_err`, `source_id`, `tag` (and `tags`), `product`, `uuid`,
  `catalogue` (by name: in that catalogue), `shape` (with `_exists_`), and attributes as
  `#name` (e.g. `#density:>10`). Unknown field → a plain error naming the closest field.
- **Times.** `2015`, `2015-03`, `2015-03-01`, full ISO; a year or month is a range
  (`start:2015` = during 2015). Relative: `now-7d` (units s, m, h, d, M, y), as in Papers'
  `added:>now-7d`.
- **Durations.** Seconds, or with a unit: `duration:>10min`, `duration:[1h TO 2h]`
  (s, min, h, d).

Additions to the rule language, all additive (existing rules and their JSON unchanged):
- `{"text": "word"}`: the any-text match a bare word makes.
- `duration` as a field of `cmp` (computed: stop − start, seconds).
- A relative time as a `cmp` value: `{"now": "-7d"}`, evaluated when the rule is evaluated.
  (A smart catalogue with one is re-evaluated whenever the project changes; it is not
  re-evaluated on a timer. Say so in the help.)
- `in_catalogue` keeps a uuid; the query shows the catalogue's name.

Printing: a rule of the form `{"filter": <all or one catalogue>, "where": P}` prints back to
a query (canonical spacing, not the user's exact text). Any other rule (the time operators:
merge, shift, tile, window, closest, set algebra) has no query form: the web app shows it
as JSON under "Advanced", as today.

## Web app

1. **The catalogue page's filter box takes the query language.** The table filters live,
   with "N of M events". A query that does not parse shows a plain one-line error under the
   box and leaves the table as it was; plain words keep working exactly as now.
2. **"Save as smart catalogue…"** appears next to the box when it holds a query. A small
   dialog: name, and "Search all events" (default) or "Only this catalogue's events". Saving
   creates the smart catalogue and opens it.
3. **A smart catalogue's page** shows its query in the box, with "Save changes" when it was
   edited (and "Revert"). A rule without a query form shows "This rule uses time operations:
   edit it as JSON" with the existing JSON editor.
4. **Builder.** A "Build a search" button by the box opens rows: field ▾, operator ▾ (fitting
   the field's type), value (dropdown for status, catalogue, author; date or number inputs
   where they fit), "all / any of these", add/remove rows. It writes the query text into the
   box; it never holds state the text does not.
5. **Presets.** In the sidebar's "+" menu, "New smart catalogue" lists: Recently added
   (`created:>now-7d`), Mine (`author:<me>`), Not reviewed yet (`NOT status:reviewed NOT
   status:final`), Low confidence (`confidence:<0.5`), With a shape (`_exists_:shape`), and
   "Empty search" (opens the builder).
6. Help page: a "Searching" section with the syntax table and examples.

## Python

- `project.search(query) -> list[Event]`.
- `catalogue.query` (str or None; None when the rule has no query form) and
  `catalogue.query = "tag:shock confidence:>=0.8"` (sets the rule to all events where …).
- `project.create_smart_catalogue(name, query, source=None)` (source: a catalogue to search
  in, else all events).
- `cocat_rs.parse_query(text) -> dict` (the rule JSON), for scripts that build rules.
- Errors: `ValueError` with the position and a plain message.

## Tests

Core: parse → predicate for every construct; print → parse round trip; precedence; errors
with positions; relative times against a fixed clock; duration; `text`; the new rule forms
evaluate as specified. Python: search, query get/set, create_smart_catalogue. Web: query in
the box filters, save as smart, edit, builder writes text, presets.

## Non-goals

Query forms for the time operators; full-text search inside attribute JSON lists beyond
contains; saving the user's exact query text (the rule is the truth, the text is printed).
