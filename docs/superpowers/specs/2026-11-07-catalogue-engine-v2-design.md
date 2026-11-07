# Catalogue engine v2: data model, smart catalogues, tscat import

Builds on `2026-11-07-tscat-convergence-brief.md` (the user's decisions). This document is the
design a builder plans from. Calls marked *(my call)* were not discussed with the user.

## Intent

cocat-rs becomes SciQLop's only catalogue engine: shared rooms on a server and local rooms
that never connect, both through `cocat_rs`. tscat is frozen. The finished product:

- a lean event layout (data model v2) that records provenance, status, confidence and
  uncertainties without making a hand-made event cost more;
- smart catalogues: a catalogue defined by a rule, evaluated whenever it is read, so it
  follows its inputs as people edit them;
- a one-shot import of a local tscat database (and tscat's JSON files) into a room.

User constraints: never make creating an event by hand painful; CRDT merges never drop an
edit; no backward compatibility with Python cocat or old rooms beyond being able to import
them; tscat rules need not keep their form, only import.

## Non-goals

Zenodo/DOI push, TFCat import/export, units conversion, UCDs, DataCite metadata, a Leptos
UI. SciQLop's plugin changes belong to the plugins session (it gets the API below).

## 1. Room layout v2

Every room holds `meta` → `layout: 2` (written by the first writer that finds it absent).
A reader that finds a non-empty room without it treats the room as v1 (see section 5).

Times are `i64` nanoseconds since 1970-01-01 UTC (`Any::BigInt`), *(my call: ns as the
brief suggested; range 1677–2262 is enough; 8 bytes instead of a 32-byte text)*.

### events: uuid → map

The uuid is the map key only (v1 repeated it inside). Always: `start`, `stop`.
Only when set (absent means the default in brackets):

| key | value |
| --- | --- |
| `author` | text (absent for model events: their origin says who) |
| `created` | ns |
| `origin` | id in `origins` [absent = made by hand] |
| `status` | `preliminary` `reviewed` `final` `rejected` `deleted` [absent = `confirmed`] |
| `confidence` | number 0–1 |
| `start_err`, `stop_err` | number, seconds, symmetric |
| `shape` | array of `[ns, frequency]` vertices (TFCat polygon); writing it sets start/stop to its time span |
| `source_id` | text, the event's id in an imported list |
| `tags`, `products` | array of text, sorted, unique *(my call: an atomic array, not a map per event: one item instead of one per entry; a concurrent tag edit on the same event resolves last-writer-wins, accepted)* |
| `#<key>` | attribute value (JSON) *(my call: attributes are flat keys with a `#` prefix, not a nested map, so two people editing different attributes of one event both win and no map is created per event)* |

`deleted` is the trash: hidden from every read unless asked for, restorable, purgeable.
`rejected` is a scientific judgement and stays visible.

### catalogues: uuid → map

`name`; only when set: `author`, `created`, `status` (`deleted` only), `tags` (array),
`columns` (array of attribute keys shown as columns), `#<key>` attributes, `rule` (section 3,
JSON). `events` stays a map used as a set (uuid → true): concurrent adds must all win.

### Other roots

- `origins`: id → map: `kind` (`rule` | `detector` | `model` | `import`), `name`, `version`,
  `author`, `created`, `used` (array of `{product, version or retrieved}`), `parameters`
  (JSON), `derived_from` (DOI or text). W3C PROV names.
- `comments`: event uuid → text. Kept out of the event map so reads of many events never
  carry long text.
- `attributes` (registry): key → `{type, unit?, description?}`, type one of `number`,
  `text`, `bool`, `date`. One definition per key per room.

### Registry rules

- Writing an attribute whose key is not registered registers it in the same transaction,
  type guessed from the value. Nothing is asked of the user.
- A value of another type than registered is written anyway; the write returns a warning
  (Python: `warnings.warn`). The registry never blocks or drops an edit.
- `date` values: Python `datetime` is stored as ISO text and registers `date`
  *(my call: JSON has no date type; the registry carries the meaning)*.

### Size target

A `cocat-core` test builds 1,000 hand-made events in v1 and in v2 (same fields: author,
two tags, one product, two attributes; v2 adds `created`) and asserts v2 bytes ≤ 1.10 × v1.
It prints both, and the bytes of a model event (origin, confidence, no author).

## 2. Engine API (cocat-core `model`)

Keep the narrow waist: the callers (server, client, Python) only use `model`. `EventRecord`
and `CatalogueSummary` grow the new fields; `rating` goes. New functions: status setters,
purge, origins (create/read), comments (get/set), registry (read, set unit/description),
catalogue columns and rule, and `existing_tags` (sorted union over non-deleted events and
catalogues). Reads skip `deleted` unless a flag asks for them.

## 3. Smart catalogues

A catalogue with a `rule` is smart. Its members are its listed events **plus** what the rule
yields (tscat's "assigned ∪ filtered").

### Rule format (JSON tree) *(my call; replaces tscat's predicate classes)*

Sets of intervals:

- `{"all": true}`: every event that is not deleted;
- `{"catalogue": uuid}`: that catalogue's members (recursive; a cycle is an evaluation error,
  not a crash);
- `{"filter": set, "where": predicate}`;
- `{"merge": set, "gap": s}`, `{"intersect": [a, b]}`, `{"subtract": [a, b]}`,
  `{"union": [a, ...]}`, `{"shift": set, "by": s}`, `{"tile": set, "width": s}`;
- `{"window": [a, b], "within": s}` (events of a within s of any of b),
  `{"closest": [a, b]}` (events of a, each with its gap to the nearest of b).

Predicates: `{"cmp": [field, op, value]}` (op `==` `!=` `<` `<=` `>` `>=`),
`{"match": [field, regex]}`, `{"has": key}`, `{"contains": [field, value]}` (tags,
products), `{"not": p}`, `{"and": [...]}`, `{"or": [...]}`, `{"in_catalogue": uuid}`.
`field` is a fixed field name or `#key` for an attribute. Regex through the `regex` crate.

Intervals are half-open `[start, stop)`. Zero-length events are points: they belong to an
interval when `start <= t < stop`. `merge` joins overlapping and touching intervals (and
those closer than `gap`). Every edge case gets a test.

### What evaluation returns

An item is an existing event (uuid) or a derived interval (`start`, `stop`, and the uuids
it came from). Filters, `window`, `closest`, `catalogue` and `all` return existing events;
the interval operators return derived intervals. Derived intervals are read-only.
`materialize(catalogue, name)` copies a smart catalogue's current result into a new static
catalogue: existing events are listed, derived intervals become new events whose `origin`
is a `rule` origin recording the rule *(my call: this is how a derived result becomes
editable and taggable, with provenance)*.

Evaluation is a pure function of the document, in Rust. It runs on every read; the target
is a filter over bench100k in under 100 ms (release), measured in a test that prints it.

### tscat predicates

`rule_from_tscat(dict)` converts tscat's predicate JSON (`Comparison`, `Match`, `Not`, `Has`,
`All`, `Any`, `In`, `UUID`, `InCatalogue`, `Field`, `Attribute`) into a rule
`{"filter": {"all": true}, "where": ...}`. `rating` comparisons become `confidence`
comparisons with the value divided by 10. Datetimes (`rhs_type: datetime`) become ns.

## 4. Python package (`cocat_rs` 0.2.0)

- Event: `status`, `confidence`, `start_err`, `stop_err`, `origin`, `source_id`, `created`,
  `comment`, `shape`, all settable; `create_event(...)` takes them as keyword arguments;
  `rating` is gone. `ev.delete()` moves to the trash; `ev.purge()` removes it;
  `room.trash()` lists deleted events; `ev.restore()`.
- Catalogue: `columns`, `rule` (dict or None; setting validates it), `materialize(name)`.
  For a smart catalogue, `event_arrays()` covers every member; a derived interval has uuid
  `None` and its sources in an extra `sources` list.
- Room: `create_origin(...)`, `origin(id)`, `attribute_types()`,
  `set_attribute_info(key, unit=None, description=None)`, `existing_tags()`.
- Listener: `event_fields(uuid, set, removed)` for the fixed fields other than the range,
  `catalogue_rule(uuid)`, plus the existing methods. A smart catalogue's members change
  when any input changes; the listener does not compute that, the plugin re-reads smart
  catalogues on `changes_done` *(my call: cheap at the measured speed)*.
- `cocat_rs.tscat`: `import_database(room)` (needs `tscat` installed; reads through tscat,
  so tscat runs its own migrations), `import_json(room, path)`, `export_json(room,
  catalogues, path)` in tscat's JSON format (no tscat needed). Uuids kept,
  `confidence = rating / 10`, removed items → `deleted`, dynamic catalogues → `rule`.
  Re-running an import updates rather than duplicates (uuids are the keys).

## 5. Old rooms and old clients

- `cocat-core` converts a v1 document into v2 updates (`migrate_v1`). The server's import
  command applies it to every room file, so production rooms move to v2 at switch-over.
- A client cache that holds v1 content is discarded on open (it is only a cache).
- The websocket requires the subprotocol `cocat.v2`. `cocat_rs` sends it; Python cocat and
  the old SciQLop plugin do not, so they are refused before they can write v1 data into a
  v2 room.

## 6. Done means

Full Rust and Python suites green; the size test and the evaluation timing pass; a tscat
database (tscat's own test fixtures) imports with every field checked; the dev server runs
v2 with its rooms migrated, and a `wss` client syncs one of them; cocat-rs 0.2.0 tagged
(push only with the user's go); the plugins session gets the API.
