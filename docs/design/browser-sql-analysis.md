# Design: ask the public catalogues anything, in the browser

Status: design note, written 2026-11-27. Steps 1 to 4 of section 6 are built: the snapshot
writer, `survey.sql`, the sandboxed engine and the two SQL tools with their query panel.
Sections 7 and 8 say what the build changed in this note.

## The goal

A visitor should be able to ask any question about the public catalogues and get an answer
in their own browser. Their browser's AI (Brave Leo, Chrome's agent) does the asking. The
only rule: no arbitrary remote code execution.

Today the page offers two WebMCP tools: search the catalogues, and open one. "Open" returns
no events. So the AI can find a catalogue but cannot analyse anything.

The plan, agreed earlier:

1. The server publishes read-only Parquet files of the public catalogues.
2. DuckDB-wasm runs in the visitor's browser and queries them.
3. New WebMCP tools: describe the data, run SQL, draw a chart.
4. The survey's method (tagging, tolerance matching, chance baseline) becomes shared SQL, so
   the AI's numbers equal the numbers on <https://sciqlop.lpp.polytechnique.fr/catalogues/>.
5. Every answer shows its query and its raw result, so people can check.

## The short version

- The snapshot is small. All 441 public catalogues (1.65 M events) fit in a 31 MB events
  file, as the server writes it. The attributes add 59 MB, split into one file per catalogue,
  loaded only when a query needs them.
- The engine runs in a sandboxed iframe with an opaque origin. It has no network at all:
  the page fetches every file and hands the bytes over. DuckDB's own network switch is off
  too, and locked.
- The tools are registered by the page, not by the iframe. WebMCP inside an opaque-origin
  iframe does not work reliably today. The page forwards each call to the iframe.
- The survey's method lives in one SQL file. The browser loads it. The survey notebook runs
  the same file with native DuckDB, on the same snapshot. I checked the hardest part: a SQL
  ASOF join gives exactly the survey's match rates (40 of 40 rates, zero difference).

## 1. Data layout

### What the server publishes

Four kinds of files, all public data, all read-only:

| File | Rows | What it holds |
|---|---|---|
| `manifest.json` | — | Snapshot version, and each file's name, size and hash. |
| `catalogues.parquet` | one per catalogue | id, name, folder, source, description, citation, relay source and URL, event count, attribute columns. |
| `events.parquet` | one per event | `event` (integer id), `catalogue`, `start`, `stop`, `status`, `confidence`, `start_err`, `stop_err`, `tags`, `observer_raw`. |
| `attributes/<catalogue>.parquet` | one per event with attributes | `_event`, `_uuid`, then the catalogue's own attribute columns, typed. |
| `survey.sql` | — | The shared rules, views and macros (section 2). |

`events.parquet` holds the catalogues one after the other, in the order of the public index,
each sorted by start. The integer `event` id joins it to the attribute files. It numbers the
events of one snapshot only: the next snapshot may number them differently. The event uuid lives only in the attribute files: as text it would more than
double the events file (see the measurements).

`observer_raw` is the one attribute pulled into the events file. It is the first of the
attributes `spacecraft`, `stereo` or `location`, which the survey needs for every mixed list.
20 catalogues carry one of them. Without it, a survey query would have to open many attribute
files.

### Heterogeneous attributes

There are 1,426 different attribute keys and 11.9 M attribute values. I compared three ways
to store them.

| Layout | Size | Verdict |
|---|---|---|
| One JSON column in the events file | 81 MB total | Every query that touches attributes reads all of them. Types are lost. |
| One `MAP(VARCHAR, VARCHAR)` column | 76 MB total | Same problem, and numbers become text. |
| Long format: (event, key, number, text) | 208 MB | Too big. Awkward SQL for a small model. |
| **One file per catalogue, real typed columns** | **56 MB in 247 files** | Chosen. Within one catalogue the columns are uniform. |

With typed columns per catalogue, a question about STIX flares reads
`attributes/public.stix.flares.parquet`, with `goes_class VARCHAR` and
`goes_flux_w_m2 DOUBLE`. The model writes plain SQL against plain columns.

The writer fixes the types so they cannot drift between snapshots:

- a JSON number is always `DOUBLE`, a boolean `BOOLEAN`, text `VARCHAR`;
- a key holding mixed types becomes `VARCHAR`;
- a time stays text. The model can cast it, and the docs say so.

### Measured sizes

I measured them on the local mirror of cocat-dev (2026-11-26 copy: 441 catalogues, 1,646,781
events). I used DuckDB 2.0.1 natively, Parquet with zstd compression.

The server writes the files with arrow-rs, and they come out larger: `events.parquet` is
31 MB, the 247 attribute files 59 MB, 93 MB in all with the rest. arrow-rs's default encodings
are less compact than DuckDB's. Delta encoding of the times and ids is the first thing to try
if the size matters.

| File (written by DuckDB) | Size |
|---|---|
| `events.parquet` (fields above, integer id, no uuid) | 20.9 MB |
| same, with the uuid as text | 50.4 MB |
| same, with the uuid as a `UUID` | 42.8 MB |
| `attributes/` (247 files; 194 catalogues have no attributes) | 55.6 MB |
| — median file / 90th percentile / largest | 21 kB / 625 kB / 6.0 MB |
| `catalogues.parquet` (from the survey's copy) | 0.1 MB |

The largest catalogues are the MMS SITL selections (126 k events), the STIX flares (108 k)
and the four Grimmich Cluster region lists (82–88 k each).

### Loading, lazily

1. The page reads `manifest.json` (`Cache-Control: no-cache`, with an ETag).
2. It fetches `catalogues.parquet`, `events.parquet` and `survey.sql`. Their names carry their
   hash, so they are cached forever (`immutable`). A returning visitor downloads nothing.
3. It hands the bytes to the engine, which registers them as in-memory files.
4. An attribute file is fetched the first time a query names it. The engine reports a
   missing file. The page fetches it, hands it over, and the query runs again.

I chose whole-file transfer over HTTP range requests, for three reasons.

- The engine then needs no network at all (section 4, rule 1).
- The files are small. The events file is fetched once and kept in the browser cache.
- DuckDB-wasm's range reads are fragile today. Its docs say registered HTTP files are "read
  lazily" ([data ingestion](https://duckdb.org/docs/stable/clients/wasm/data_ingestion#parquet)).
  But its C++ defaults to full reads, `force_full_http_reads.value_or(true)`
  ([web_filesystem.cc](https://github.com/duckdb/duckdb-wasm/blob/main/lib/src/io/web_filesystem.cc)).
  Its troubleshooting page also lists range requests failing on Firefox
  ([troubleshoot](https://duckdb.org/docs/stable/clients/wasm/troubleshoot)).

The server's static file service already answers range requests (tower-http `ServeDir`
sends `Accept-Ranges` and `206`). So we can switch to ranges later if the files grow.

### Refresh and versioning

The server already keeps one JSON-lines file of live events per public catalogue. It rewrites
that file when the room changes (`public_search.rs`, the `public-index` folder). The Parquet
writer reads those files. It does not load rooms again.

- The server rewrites the snapshot when it starts, then every hour, if a catalogue or
  `survey.sql` changed. The snapshot's version is a hash of every catalogue's file stamp and
  index entry, and of `survey.sql`. A rewrite takes about 30 s for all 441 catalogues.
- Each file is named by a hash of its bytes. An attribute file whose catalogue did not change
  keeps its name, so a browser keeps its cached copy.
- Each rewrite writes the new files, then swaps `manifest.json`. A page that loaded an older
  manifest keeps working on the older files until it reloads. The files of the last two
  manifests are kept.
- `manifest.json` carries the snapshot time. The query panel shows it ("data as of …").
- Routes: `GET /api/public/snapshot` gives the manifest; `GET /api/public/snapshot/<name>`
  gives a file. Both need a signed-in account, like `/api/public/events`.

Writer crates: [`arrow`](https://crates.io/crates/arrow) and
[`parquet`](https://crates.io/crates/parquet) (arrow-rs). The server does not need DuckDB.

### Memory in the browser

- The buffers: 31 MB of events, plus each attribute file opened (6 MB at most).
- The engine queries the Parquet bytes directly. It never copies them into tables. Loading
  all events into a DuckDB table took 520 MB natively, far too much for a tab.
- `memory_limit` is set to 1 GB. DuckDB-wasm honours it: its spill tests set 128 MB
  ([spill.test.ts](https://github.com/duckdb/duckdb-wasm/blob/main/packages/duckdb-wasm/test/spill.test.ts)).
  WebAssembly caps the heap at 4 GB (`MAXIMUM_MEMORY=4GB`,
  [lib/CMakeLists.txt](https://github.com/duckdb/duckdb-wasm/blob/main/lib/CMakeLists.txt)).
- Natively, events per year over all 1.65 M events took 16 ms. I did not measure wasm.

## 2. The shared SQL: one source of truth

### Where the truth lives

The survey's method lives in **one SQL file, `survey.sql`**, kept in cocat-rs. The browser
loads it. The survey notebook runs the same file with native DuckDB.

I considered generating SQL from Rust and rejected it.

- The rules are data: ordered regular expressions, overrides, tolerances. A person edits
  them at each survey run, then checks that no catalogue moved. In SQL that is one edit and
  one re-run. In Rust it is an edit, a build, a release and a deploy.
- Both engines run the file as it is. With Rust there would be three languages, and the
  survey would still need a way to run the result.
- The server never needs the tags itself. If it ever does, the right move is to let it read
  them from a snapshot, not to port the rules.

The rules move into the file as tables, so they stay data:

```sql
CREATE TABLE theme_rules (rank INTEGER, pattern VARCHAR, theme VARCHAR);
CREATE TABLE observer_rules (rank INTEGER, pattern VARCHAR, observer VARCHAR);
CREATE TABLE observer_names (rank INTEGER, pattern VARCHAR, observer VARCHAR);  -- per-event spacecraft names
CREATE TABLE overrides (name VARCHAR, theme VARCHAR, observer VARCHAR);
CREATE TABLE tolerances (theme VARCHAR, hours DOUBLE);   -- the survey's TOLERANCE_H; default 1 h
CREATE TABLE chance_shifts (days DOUBLE);                -- -61.9, -19.1, 13.3, 40.7
```

There was one trap. DuckDB uses RE2 regular expressions and the survey used Python's `re`. The
one-off check passed: RE2 gives the same phenomenon, observer, label and source for all 441
catalogues. The Python rules are deleted.

Both sides must run the same DuckDB version: the wasm build pins one, and the notebook pins
the matching `duckdb` Python package.

### What `survey.sql` defines

Tables and views, ready to query. The two per-catalogue tables are computed once, when the
file runs (1.5 s natively). Recomputing them in every query made the matching ten times
slower.

| Name | Columns beyond the raw files | Survey step |
|---|---|---|
| `catalogue_tags` (table) | `theme`, `observer`, `short_source`, `label`, `n`, `first`, `last`, `covers` (share of its span covered), `wrapped` | 2, 5 |
| `clean_events` (view) | `start`/`stop` with the 2^32-second wrap undone, `observer` (per event), `catalogue_observer`, `theme`, `label`, `duration_h`, `start_h`/`stop_h`, `repaired` | 2, 3 |
| `coverage` | per theme and observer: catalogues, events, first and last year, median duration | 5 |
| `single_list_pairs` | phenomenon and observer pairs held by one catalogue | 5 |
| `exact_copies` | pairs of catalogues sharing at least 20 % of events to the minute | 6 |

Macros:

```sql
-- scalar
theme_of(folder VARCHAR, name VARCHAR) -> VARCHAR
observer_of(folder VARCHAR, name VARCHAR) -> VARCHAR
tolerance_h(theme VARCHAR) -> DOUBLE

-- table
match_rates(phenomenon := NULL, seen_at := NULL, a := NULL, b := NULL,
            which_themes := 'same', after := NULL, before := NULL)
    -> per ordered pair and shift: compared, share
agreement(a := NULL, b := NULL, phenomenon := NULL, seen_at := NULL,
          which_themes := 'any', after := NULL, before := NULL)
    -> (theme, observer, a_catalogue, b_catalogue, label_a, label_b, theme_b,
        compared, matched, chance, lift)
agreement_matrix(phenomenon, seen_at, after := NULL, before := NULL)
    -> one agreement row per ordered pair of catalogues of one phenomenon at one observer
same_events(phenomenon := NULL, seen_at := NULL)
    -> both-way pairs: matched either way >= 50 %, lift >= 3, >= 10 events (the page's table)
confirmations(phenomenon, seen_at)
    -> per catalogue: events_checked, only_here, only_here_if_shifted, confirmed_by_half, ...
events_per_year(phenomenon := NULL, seen_at := NULL)
    -> (theme, year, events)
attributes(id)
    -> the catalogue's clean events joined with its attribute file
```

`a` and `b` are catalogue ids or exact labels. The tolerance is the phenomenon's.
`which_themes` is `'same'` (the survey's comparison), `'different'` (links between
phenomena) or `'any'`. `after` and `before` cut both lists to a period. The survey has no such
cut, so with them left out the macros give the page's numbers. The observer parameter is
`seen_at`, because `at` is a keyword in DuckDB.

The planned `found_in` macro is not there: `agreement` with `a` and `b` answers the same
question. One check stays in Python: the survey's test of the date repair compares only the
repaired events of a list, without the span cut, so it keeps its own 15 lines.

### The matching, in SQL

The survey matches with numpy. It sorts list B by start and keeps a running latest stop.
Then, for each event of A, it searches for B's last start before A's stop plus the
tolerance. The event matches when that running stop reaches A's start minus the tolerance.

That is exactly an ASOF join
([ASOF join guide](https://duckdb.org/docs/stable/guides/sql_features/asof_join)). Here is the
core of `found_in`, on a table `ev` of times in hours:

```sql
CREATE MACRO found_in_core(a, b, tol_h, shift_h) AS TABLE
WITH idx AS (
    SELECT start_h, max(stop_h) OVER (ORDER BY start_h) AS latest_stop
    FROM ev WHERE catalogue = b),
span AS (SELECT min(start_h) AS first, max(latest_stop) AS last FROM idx),
mine AS (
    SELECT start_h + shift_h AS start_h, stop_h + shift_h + tol_h AS reach
    FROM ev, span WHERE catalogue = a AND start_h BETWEEN first - tol_h AND last + tol_h)
SELECT count(*) AS compared,
       avg((coalesce(idx.latest_stop, -1e18) >= mine.start_h - tol_h)::INT) AS matched
FROM mine ASOF LEFT JOIN idx ON mine.reach >= idx.start_h;
```

I ran this against the survey's own code, on the seven Earth bow-shock lists after 2010.
Each ordered pair was compared unshifted and with the four chance shifts: 40 rates in all.
Every rate was identical: the same counts, and a difference of 0.0. Natively it took 0.25 s,
against 0.13 s for numpy. (Scratch script, not committed.)

The window must use the default RANGE frame, as above, not `ROWS UNBOUNDED PRECEDING`. With
ROWS, events that start together get different running stops, and the ASOF join may pick the
wrong one. numpy's search goes past all of them. The first sketch had ROWS and was right only
by luck.

In `survey.sql` the join carries integer unit and pair numbers, not text. With text, matching
every same-phenomenon pair at once took over 6 GB. With numbers it takes 4.5 s and about
3.7 GB at a 2 GB DuckDB limit, natively. The cross-phenomenon links (survey section 10) take
37 s, one phenomenon at a time. A browser must never ask for all pairs at once: the tools
should send one phenomenon, or one pair.

Table macros are in the [CREATE MACRO docs](https://duckdb.org/docs/stable/sql/statements/create_macro).

### Worked examples

The numbers below come from the 2026-11-26 survey data. They show what the AI would get. All
five queries were run as written on the server's snapshot, and gave these numbers.

**"Do the Cluster bow shock lists agree after 2010?"**

```sql
SELECT * FROM agreement_matrix('Bow shocks', 'Earth (magnetosphere)', after := DATE '2010-01-01')
WHERE label_a ILIKE '%cluster%' OR label_b ILIKE '%cluster%';
```

- Nguyen 2021 (Cluster 1): 9 % of its 414 crossings are in the SHARP Cluster list. By chance
  it would be 1 %.
- SHARP Cluster: 54 % of its 54 crossings are in Nguyen 2021. By chance it would be 4 %.
- So they agree well above chance. But SHARP lists far fewer crossings after 2010.
- TT_BOW_CLUSTER has no events after 2010.

**"Which observers have no ICME list?"**

```sql
SELECT observer FROM catalogue_tags
WHERE theme NOT LIKE 'Operations%' AND observer NOT IN ('Unknown', 'Many')
GROUP BY observer
HAVING count(*) FILTER (WHERE theme = 'ICMEs, magnetic clouds, sheaths') = 0;
```

The answer is Earth (magnetosphere), JUICE, Saturn, Sun (remote sensing), and Voyager 1 and 2.

"Earth" needs a word. ICMEs that reach Earth are listed under L1 (near-Earth solar wind),
which has many lists. The schema notes must say this, or the model will report a false gap.

**"Events per year of foreshock transients at Mars"**

```sql
SELECT * FROM events_per_year('Foreshock transients', 'Mars');
```

There are three TUMS lists. SLAMS dominate: 1,769 in 2015, then 3,330, 3,475, 6,928 and
5,187 in 2019. Hot flow anomalies run 4 to 25 a year from 2014 to 2020.

Two HFAs are dated 1988. No spacecraft was at Mars then: Phobos 2 arrived in January 1989.
The raw rows under the answer let a user spot this.

**Outside the survey: "Do bigger flares last longer?"**

```sql
SELECT left(goes_class, 1) AS class, count(*) AS flares,
       round(median(duration_h) * 60, 1) AS median_minutes
FROM attributes('public.stix.flares')
WHERE goes_class IS NOT NULL
GROUP BY class ORDER BY class;
```

| class | flares | median minutes |
|---|---|---|
| A | 1,339 | 3.5 |
| B | 30,063 | 4.3 |
| C | 70,650 | 5.6 |
| M | 2,739 | 18.1 |
| X | 127 | 43.5 |

The model would then call `plot` with a bar chart of `median_minutes` by `class`.

**Outside the survey: "Which catalogue first listed the most events of 2003?"**

```sql
SELECT c.name, count(*) FROM clean_events e JOIN catalogue_tags c USING (catalogue)
WHERE year(e.start) = 2003 GROUP BY ALL ORDER BY 2 DESC LIMIT 10;
```

This needs no macro at all. Many questions will look like this.

### How the survey page reuses it

The notebook keeps its charts and its hand-written findings. Only its data and method
changed.

1. It downloads the snapshot from the server: `manifest.json`, then the files whose hash
   changed. It no longer opens every room through cocat-rs.
2. It opens native DuckDB (2.0.1, the browser engine's version) and runs the
   `survey.sql` that came with the snapshot. `COCAT_SURVEY_SQL` points it at a local copy
   while the rules are edited.
3. Each section is a query: `catalogue_tags`, `clean_events`, `coverage`,
   `single_list_pairs`, `exact_copies`, `agreement()`, `same_events()`, `confirmations()`.
   pandas only receives results to plot.
4. A run takes 66 s and 5 GB at most, against 58 s and 2.2 GB before.

To get the page's numbers to the last bit, `survey.sql` copies three pandas habits:

- a duration in seconds is whole seconds plus the microseconds over 1e6 (`seconds()`);
- a median is rounded half to even (`round_even`);
- the chance rate adds the four shifted rates in order, then divides (`chance_share()`).

The check: the old notebook on its own data, against the new one on the server's snapshot.
Every table matched: catalogues and tags, all 1.65 M events, coverage, single lists, all 2,340
pairs, the 358 same-event pairs, 145 confirmations, 112 copies, 318 links, per-year counts,
the 22 added catalogues. On the page, every number is the same. Three things moved, none of
them a number:

- the date in the footer;
- 164 of the 358 same-event pairs show A and B the other way round. The old run ordered a
  pair by catalogue uuid, and the snapshot names catalogues by public id;
- in two confirmation charts, bars tied at 0 % come in another order.

`crates/cocat-py/tests/test_survey_sql.py` runs `survey.sql` on a small snapshot made in the
test: tags, per-event observers, the date repair, matching, same events, events per year. A
browser test that runs the same queries in DuckDB-wasm comes with step 3.

## 3. Tools

The page registers five tools through WebMCP's `document.modelContext.registerTool`
([spec](https://webmachinelearning.github.io/webmcp/)). It forwards three of them to the
sandboxed engine with `postMessage`.

| Tool | Input | Output | Caps |
|---|---|---|---|
| `search_public_catalogues` | as today | as today | — |
| `open_public_catalogue` | as today | as today | — |
| `describe_data` | `catalogue` (optional) | without input: tables, columns with meanings, views and macros with signatures, the theme and observer vocabularies, example questions with their SQL, the snapshot version. With a catalogue: its attribute columns, types and 3 sample values each. | ~3 k tokens without input |
| `run_sql` | `sql`, `max_rows` (default 50) | `columns`, `types`, `rows`, `row_count`, `truncated`, `elapsed_ms`, `query_id`, `snapshot` | `max_rows` ≤ 500; text cells cut at 200 characters; answer ≤ 64 kB; 15 s; 1 GB |
| `plot` | `query_id` or `sql`, `spec` (Vega-Lite, without data) | `shown`, `rows_plotted`, `warnings` | ≤ 5,000 rows plotted |

Every tool sets `readOnlyHint: true`. `run_sql`, `describe_data` and the search set
`untrustedContentHint: true`, because catalogue text is written by third parties. Both
annotations are in the spec's tool descriptor.

Results are a JSON string. Brave's Leo reads tool results as text. The spec serializes
whatever `execute` returns to a JSON string, so text works for both.

### Helping a small model (Leo)

A small model fails on guessing: table names, theme spellings, the ASOF join. So the tools
remove the guessing.

- `describe_data` gives the exact theme and observer strings. A `WHERE theme = 'ICME'` then
  returns nothing, and the error hint says to use the listed values.
- The hard parts are macros. The model never writes an ASOF join or a chance baseline.
- Eight example questions, each with its SQL, cover the common shapes: count, per year,
  agreement, gaps, attributes, plot.
- Errors come back as answers, not exceptions, each with a hint. A binder error says "call
  `describe_data`". A missing attribute file is fetched by the page and retried, so the model
  never sees it.
- Notes the model must relay: times are UTC; a match rate means nothing without its chance
  rate; "Earth" means the magnetosphere and L1 means the solar wind near Earth.
- The tool descriptions say: show the query to the user, and report the numbers as they came
  back.

### What the user sees

The page, not the model, shows each query in a "Queries" panel. Each entry has:

- the SQL;
- the full result table, scrollable, with a CSV download;
- the chart, if `plot` drew one;
- the snapshot version, and a "Run again" button.

The model cannot hide or reword a query: the page shows what really ran. Cells are plain
text, with no HTML.

## 4. Security

The engine is a throwaway sandbox. The SQL is untrusted code, and DuckDB's own
[securing guide](https://duckdb.org/docs/stable/operations_manual/securing_duckdb/overview)
treats it that way: "Treat SQL in DuckDB like code in Bash or Python." What follows makes
sure that code can reach nothing.

### Rule 1: no network from SQL

Two independent locks.

**DuckDB settings**, applied after the files are registered and before the first query, in
the order the securing guide requires:

```sql
SET allowed_paths = ['events.parquet', 'catalogues.parquet', 'attributes/…'];
SET autoload_known_extensions = false;
SET autoinstall_known_extensions = false;
SET allow_community_extensions = false;
SET enable_external_access = false;
SET lock_configuration = true;
```

- Once external access is off, `read_parquet`, `COPY` and `ATTACH` cannot reach anything
  outside the allowed list. The allowed list itself can no longer change.
- On DuckDB 2.x, add `SET allow_extension_repositories = 'forbidden'` (new in 2.0, same guide).
- ~~The bundle links json, parquet and icu statically.~~ Wrong (section 8): the published
  builds link only `core_functions` and `nanoarrow`, and fetch everything else at run time.
  We serve the Parquet extension ourselves and the page hands it to the engine (section 9).
- Autoload is on by default in wasm
  ([extensions](https://duckdb.org/docs/stable/clients/wasm/extensions)), hence the explicit
  off.

One thing to check first. Core DuckDB refuses every path outside `allowed_paths` once
external access is off. That should include registered in-memory files, so they must be in
the list ([config.cpp](https://github.com/duckdb/duckdb/blob/v2.0.1/src/main/config.cpp)).
Attribute files arrive later. So either all 247 names go in the list up front (they are in
the manifest), or `allowed_directories` covers `attributes/`.

**The iframe's CSP**, sent as a header with the sandbox page:

```
Content-Security-Policy: sandbox allow-scripts; default-src 'none';
  script-src 'sha256-<bootstrap>' blob: 'wasm-unsafe-eval'; worker-src blob:;
  connect-src blob:; img-src blob: data:; style-src 'sha256-<style>'
```

The engine receives every byte by `postMessage`. It turns the bytes into `blob:` URLs for the
worker, the wasm and Vega. So `connect-src` needs no host at all, not even ours. A
`read_parquet('https://…')` that slipped past DuckDB would still be refused by the browser.
WebAssembly needs `'wasm-unsafe-eval'` under a CSP
([MDN script-src](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Content-Security-Policy/script-src)).

**Tests**

- `SELECT * FROM read_parquet('https://example.org/x.parquet')` fails.
- So do `read_csv('/etc/passwd')`, `ATTACH 'x.db'`, `COPY … TO 'x'`, `INSTALL httpfs` and
  `LOAD httpfs`.
- `SET enable_external_access = true` fails, and so does `RESET`.
- A network log of the iframe shows no request at all.

### Rule 2: the engine is cut off from the user's session

- `<iframe sandbox="allow-scripts">`, without `allow-same-origin`. The frame gets an opaque
  origin, "a special origin that always fails the same-origin policy"
  ([MDN iframe](https://developer.mozilla.org/en-US/docs/Web/HTML/Reference/Elements/iframe)).
  It has no cookies, no storage, and no access to the page.
- The same rule is repeated as the CSP `sandbox` header. That directive works only as a
  header ([MDN sandbox](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Content-Security-Policy/sandbox)).
- The page talks to the frame only through `postMessage`. The page checks
  `event.source === frame.contentWindow`. The frame answers only its parent.

**What DuckDB-wasm needs, and how it fits**

- **Threads.** The threaded build needs cross-origin isolation (COOP and COEP headers) for
  SharedArrayBuffer. The single-threaded build needs only WebAssembly exception handling
  ([platform.ts](https://github.com/duckdb/duckdb-wasm/blob/main/packages/duckdb-wasm/src/platform.ts),
  [deploying](https://duckdb.org/docs/stable/clients/wasm/deploying_duckdb_wasm)). An opaque
  sandboxed frame is not cross-origin isolated anyway (HTML spec, `cross-origin-isolated`
  policy, default `self`). So we use the single-threaded build. No COOP/COEP, and nothing
  changes in nginx: the server sets every header.
- **The worker.** A worker script must be same-origin, which an opaque origin never is
  ([HTML: fetch a classic worker script](https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-classic-worker-script)).
  A `blob:` worker inherits its creator's origin and works
  ([MDN Worker()](https://developer.mozilla.org/en-US/docs/Web/API/Worker/Worker)). DuckDB's
  docs use the same Blob wrapper ([overview](https://duckdb.org/docs/stable/clients/wasm/overview)).
- **OPFS.** It is not needed: we use no `opfs://` path, and spill goes to `js_buffer://tmp`
  ([instantiation](https://duckdb.org/docs/stable/clients/wasm/instantiation#configuration)).
  But see the version risk below. Builds from dev110 on mount OPFS on open, and fail in an
  opaque origin.
- **Our serving.** The sandbox page and the engine assets are files of the web bundle, served
  under `/ui/app/` by `ServeDir`. The sandbox page needs its own CSP header. That is one more
  header in the server's file route. nginx passes it through.

**Tests**

- In the frame, `document.cookie` throws or is empty.
- `fetch('/api/public')` from the frame is refused.
- `parent.document` throws.
- A message from another window gets no answer.

### Rule 3: public data only, published fields only

- The writer takes catalogues from the public index only: rooms whose id starts with
  `public.`, minus relayed lists no longer published (the same filter the survey's mirror
  uses).
- Each file has a fixed list of allowed fields. No owner, no member, no room outside the
  index.
- Events drop `author`, `created`, `origin`, `products` and `shape`. In people's rooms,
  `author` holds an email address.

**Test.** Build a database holding:

- a private room with an event authored `secret@example.org`;
- a shared room;
- a public catalogue with an event authored by a person.

Write the snapshot. Read every file back as bytes. Expect no `example.org` in any byte, no
private room id, and no column outside the allowed list.

### Rule 4: runaway queries

- The engine runs in a worker, so the page never freezes.
- `memory_limit = '1GB'`, set before the lock.
- **Timeout.** Queries go through `send()`, which can be cancelled. `query()` cannot.
  After 15 s the frame calls `conn.cancelSent()`
  ([async_connection.ts](https://github.com/duckdb/duckdb-wasm/blob/main/packages/duckdb-wasm/src/parallel/async_connection.ts)).
  The cancel only lands between polling slices
  ([webdb.cc](https://github.com/duckdb/duckdb-wasm/blob/main/lib/src/webdb.cc)). So 5 s later
  the page drops the whole iframe and makes a new one. The page still holds the buffers, so
  the restart takes about a second.
- **Row cap.** Rows are streamed and cut at `max_rows`, so a 10 M-row cross join never
  reaches the page in full.

**Tests**

- A cross join of `events` with itself is cancelled within 20 s, and the next query works.
- A query building a 2 GB string fails with an out-of-memory error, not a crashed tab.
- `max_rows = 10_000` is clamped to 500.

### Rule 5: charts

- Vega runs in the frame, in CSP-safe interpreter mode. With vega-embed, `ast: true` is
  enough: the interpreter is bundled
  ([interpreter docs](https://vega.github.io/vega/usage/interpreter/),
  [vega-embed README](https://github.com/vega/vega-embed)).
- One custom loader whose `sanitize` always rejects. That blocks data URLs, images and
  `href` link clicks in one place: Vega swallows the rejection
  ([vega-loader](https://github.com/vega/vega/blob/main/packages/vega-loader/README.md),
  [Handler.js](https://github.com/vega/vega/blob/main/packages/vega-scenegraph/src/Handler.js)).
- The page strips the spec first:
  - `usermeta`, because vega-embed merges `usermeta.embedOptions` over the host's options,
    and the spec wins ([embed.ts](https://github.com/vega/vega-embed/blob/main/src/embed.ts));
  - `data.url`;
  - `href` encodings.
- vega-embed options: `actions: false`, `tooltip: false`, and the SVG renderer.
- Data arrives as JSON `values` only. Vega's CSV parser builds code with `new Function`, which
  the CSP refuses.

**Tests**

- A spec with `data.url: "https://example.org"`, an `href` channel, or
  `usermeta.embedOptions.ast: false` draws without any request, and with no eval CSP report.

### Rule 6: self-hosted, pinned assets

The engine and Vega files are part of the web bundle. They are fetched by the page from our
server, pinned by exact version and checked by hash. There is no CDN at run time.
`getJsDelivrBundles()` is not used: the bundles are passed by hand, as the
[self-hosting docs](https://duckdb.org/docs/stable/clients/wasm/instantiation#statically-served)
describe.

| Asset | Raw | Brotli / gzip |
|---|---|---|
| DuckDB-wasm `eh` wasm (1.33.1-dev57.0) | 35.9 MB | 5.3 MB br |
| DuckDB-wasm worker JS | 0.77 MB | 0.19 MB gz |
| vega 6.4.0 (min) | 521 kB | 180 kB gz |
| vega-lite 6.5.0 (min) | 258 kB | 82 kB gz |
| vega-embed 7.4.0 (min, interpreter included) | 61 kB | 21 kB gz |

The page fetches these only when the user opens the analysis panel, or when an agent first
calls a tool. A first visit downloads about 6 MB of code and 21 MB of data. The server should
send the wasm pre-compressed (`ServeDir::precompressed_br`).

### Rule 7: results are escaped

The page builds result tables with text nodes (Leptos text), never `inner_html`. A test
renders a cell holding `<img src=x onerror=alert(1)>` and finds it as text.

### Rule 8: prompt injection

Catalogue names, descriptions and attribute text are written by third parties. The spec has
a section on this: [prompt injection](https://webmachinelearning.github.io/webmcp/#prompt-injection)
and the [untrusted annotation](https://webmachinelearning.github.io/webmcp/#mitigation-untrusted-annotation).

- Every tool is read-only. The worst an injected instruction can do is run more read-only
  queries on public data.
- Results carrying third-party text are marked `untrustedContentHint`.
- The page shows every query that ran, so a hidden query is not possible.
- There is no tool that writes, sends, or navigates outside the page. `open_public_catalogue`
  only switches the view.

## 5. Risks and open questions

**WebMCP inside iframes is not settled.**

- The spec's `tools` permission defaults to `self`.
- `executeTool()` rejects tools of opaque-origin documents
  ([issue #349](https://github.com/webmachinelearning/webmcp/issues/349)).
- Exposure to the browser's own agent is "implementation-defined", with open issues
  [#389](https://github.com/webmachinelearning/webmcp/issues/389) and
  [#317](https://github.com/webmachinelearning/webmcp/issues/317).
- So the page registers the tools, and the frame only computes. This works whatever the spec
  decides.

**DuckDB-wasm versions are in flux.**

- npm `latest` is `1.33.1-dev57.0` (DuckDB 1.4.4). The last numbered release is 1.32.0
  (2025-12).
- npm `next` is `1.33.1-dev124.0` (DuckDB 2.0.1). It opens OPFS on every `db.open()`, which
  fails in an opaque origin ([#2302](https://github.com/duckdb/duckdb-wasm/issues/2302)). The
  fix ([#2313](https://github.com/duckdb/duckdb-wasm/pull/2313)) is not merged.
- ~~Start on `1.33.1-dev57.0`.~~ Built on `1.33.1-dev124.0` (DuckDB 2.0.1) instead, with
  `duckdb==2.0.1` in the notebook and CI: dev57 is DuckDB 1.5.4, not 1.4.4, and needs ICU
  and Parquet from the network. The OPFS failure is avoided in the frame: the worker is
  started without `navigator.storage.getDirectory`, so DuckDB does not ask (section 8).
- Write the macros in syntax both versions accept. 2.0 warns on `->` lambdas.

**DuckDB's settings on registered files are unverified.** Whether `enable_external_access =
false` plus `allowed_paths` admits registered buffers is read from core source, not tested in
wasm. It is the first thing the prototype checks. If it fails, the CSP still blocks the
network. But then local file functions need their own guard.

**Matching everything at once is heavy.** See the matching section: gigabytes natively for
all pairs. `memory_limit` keeps DuckDB's buffers in check, but the process grows past it. The
tools must ask for one phenomenon or one pair, and their descriptions must say so.

**Typed attribute columns can change type** when a source changes. The fixed type rules limit
this. A query that worked yesterday can still fail on a renamed column. The answer shows the
error and the snapshot version.

**Regex dialects.** RE2 and Python `re` differ at the edges. The one-off equality check covers
today's rules. New rules are written and tested in SQL only.

**Who may download the snapshot?**

- `/api/public/events` needs a signed-in account today. So the snapshot sits behind sign-in
  too, and the page fetches it with the session cookie.
- Making it anonymous would let the public `/catalogues/` page link straight to the analysis.
  That is a decision about the sources' terms, not a technical one.
- Each catalogue's citation and licence text travels in `catalogues.parquet`, so an answer
  can cite its lists.

**The model can still misread a right number.** The macros give correct rates. The model may
still drop the chance rate, or confuse L1 with Earth. Mitigations:

- the notes in `describe_data`;
- answers carry `chance` next to `matched`;
- the query panel shows the raw table beside the model's prose.

**Weight.** About 27 MB on the first visit. That is fine on a desktop. On a phone it is
heavy, and the panel should say so before it loads.

## 6. What to build first

1. **The snapshot writer** in cocat-server: Parquet from the existing JSON-lines files, the
   manifest, and the rule-3 test on a database with private rooms. *Built.*
2. **`survey.sql`**: rules as tables, the views and macros, and the RE2 equality check.
   Then switch the survey notebook to the snapshot plus `survey.sql`. The page's numbers must
   not move. *Built: they did not.*
3. **The sandbox engine**: iframe, CSP, DuckDB-wasm, the settings lock. Start with the rule-1
   and rule-4 tests, run headless in Chromium through Playwright. Check the `allowed_paths`
   question on day one.
4. **`describe_data` and `run_sql`**, with the query panel. Try them with Leo in Brave, with
   the WebMCP flag.
5. **`plot`**, with the Vega lockdown and its tests.

## 7. What the build changed in this note

- Sizes: the server's files are larger than DuckDB's (31 MB of events, not 21 MB).
- Refresh: the whole snapshot is rewritten when anything changed, at start and hourly, not
  file by file. Unchanged files keep their hashed names.
- `catalogue_tags` is a table, not a view, and a `catalogue_rules` table holds the rules'
  answer per catalogue.
- Macro names and parameters: `seen_at` for the observer, `which_themes`, `match_rates`; no
  `found_in`.
- The ASOF window needs the RANGE frame; the matching carries numbers, not text.
- pandas parity needs three small macros (durations, rounding, the chance mean).

## 8. What steps 3 and 4 changed in this note

Built: the engine (`crates/cocat-web/sql-engine/`), its pinned third-party files
(`fetch-duckdb.sh`), the page's side (`bridge.js`), the tools (`src/sql_tools.rs`,
`views/webmcp.rs`) and the query panel (`views/queries.rs`). Tested in headless Chromium
(`crates/cocat-web/e2e`, 26 tests) and with Brave 158's own WebMCP.

What the note had wrong, and what was done instead:

- **No DuckDB-wasm build reads Parquet offline.** `1.33.1-dev57.0` is DuckDB 1.5.4 and links
  only `core_functions`; `1.33.1-dev124.0` (DuckDB 2.0.1) links `core_functions` and
  `nanoarrow`. Parquet, JSON and ICU come from extensions.duckdb.org when first used, and the
  frame's CSP refuses that (seen: the ICU download blocked). So the server writes each table a
  second time as Arrow IPC, and the frame reads it with `read_arrow`.
- **nanoarrow cannot read compressed Arrow files** ("Compression type … not supported by this
  build of nanoarrow"). The Arrow files are uncompressed; the catalogue and status columns are
  dictionaries. On the mirror: `events.arrow` is 98 MB on disk and about 18 MB gzip on the
  wire (the server's HTTP compression). The snapshot is 389 MB on disk with both formats.
- **survey.sql names no file.** Each host defines `catalogues`, `events` and
  `attribute_rows(id)` before it runs: Parquet in Python, Arrow in the browser. Its date-repair
  limit avoids ICU (`make_timestamp(epoch_us(now()))`).
- **DuckDB 2.0.1, not 1.4.4,** with the OPFS bug (#2302) side-stepped: the frame starts the
  worker with `StorageManager.prototype.getDirectory` undefined; DuckDB then skips OPFS, which
  an opaque origin does not have anyway. Python follows: `duckdb==2.0.1`. Under 2.0.1 the
  page's numbers still match the old notebook's; the median needed numpy's rounding written
  out (`round_even(x * 100, 0) / 100`).
- **The `allowed_paths` question:** files registered before or after the lock are readable
  through `read_arrow` with `allowed_paths` (events, catalogues) and `allowed_directories`
  (`attributes/`). A bare file name (`FROM 'x.arrow'`) is no longer resolved after the lock:
  queries name `read_arrow(...)` or the views.
- **`INSTALL` is a no-op in DuckDB-wasm:** it answers Success and downloads nothing, even with
  `allow_extension_repositories = 'forbidden'`. `LOAD` fails, and the test checks both.
- **Freshness:** the snapshot was served stale for up to an hour. Now a manifest request that
  sees changed catalogues starts the rewrite in the background.
- **Out of memory:** DuckDB's own "Out of Memory Error" leaves the engine usable; only a
  WebAssembly trap restarts it. A start that never answers fails after 60 s.
- **Memory in the tab is higher than planned.** On the mirror, Chromium's renderer and worker
  processes held 1.1 GB once the engine started (the debug app bundle, the 98 MB events file,
  DuckDB), and 2.2 GB after matching every pair of the plasma-region lists. The 1 GB
  `memory_limit` bounds DuckDB's buffers, not the WebAssembly heap, which never shrinks. The
  tab stayed alive. Loading the Parquet extension from our own server (by answering the
  worker's download itself) would cut the events file to 31 MB: the next thing to try.
- **Timings on the mirror:** the first call starts the engine in about 7 s; most questions
  answer in under 1.5 s; one phenomenon's full agreement matrix takes up to 9 s.

## 9. Tab memory, and the Parquet extension served by us

Measured on the mirror (441 catalogues, 1.65 M events), headless Chromium, the sum of the
PSS of its processes. Same three steps each time: the engine's first query, describe_data's
query, then `agreement_matrix('Plasma regions (where the spacecraft is)', 'Earth
(magnetosphere)')` (138 pairs, the largest matrix that fits in 1 GB).

| Step | Arrow (bdc0a0f) | Parquet + recycling |
|---|---|---|
| Page loaded | 245 MB | 252 MB |
| Engine started | 1,130 MB | 1,035 MB |
| After describe_data | 1,130 MB | 1,035 MB |
| Right after the matrix | 2,215 MB | 2,029 MB |
| 3 s after the matrix | 2,215 MB (stays) | 494 MB |
| After 2 min idle | 2,215 MB (stays) | page only |

What we learned:

- **The Arrow copy was not the main cost.** Dropping it saved about 100 MB. The rest is
  DuckDB's WebAssembly memory, which grows with a query and never shrinks: about 490 MB once
  survey.sql has run (its per-catalogue windows over every event), 1,065 MB after the matrix.
  DuckDB itself reports almost nothing held: the memory is free inside the heap, not given
  back.
- **Only dropping the engine gives it back.** So the page drops it after a query that left
  its heap past 640 MB, and after 2 minutes without a query (`RECYCLE_MB`, `IDLE_MS` in
  `sql_tools.rs`; the engine reports its heap size with each answer). The next query starts
  a new engine: about 6 s.
- **The extension, served by us.** DuckDB loads an extension with one synchronous
  XMLHttpRequest from its worker. The worker's prelude replaces XMLHttpRequest: it answers
  only an extension the page handed over (once), and refuses every other URL. The page
  fetches `parquet.duckdb_extension.wasm` from our own bundle and checks its sha256 against
  the one pinned in `bridge.js`, as for DuckDB's other files; a mismatch starts no engine
  (tested). `fetch-duckdb.sh` downloads it from extensions.duckdb.org at build time
  (v2.0.1, wasm_eh), pinned by the same hash. DuckDB checks its signature too.
- **The files shrank.** Parquet only, with the event number, start and stop as deltas (they
  were dictionaries of mostly distinct values): `events.parquet` 32 MB → 11.7 MB, the
  whole snapshot 406 MB → 68 MB on disk.
- **The engine stays a dev build.** npm's own `latest` tag is a dev build (1.33.1-dev57.0);
  the last tagged release, 1.32.0 (December 2025), is DuckDB 1.4.3. It starts in the same
  memory, but it runs out of its 1 GB on the plasma-region matrix that 2.0.1 answers, and it
  lacks `allow_extension_repositories`. So: 1.33.1-dev124.0 (npm `next`, DuckDB 2.0.1),
  pinned by hash, as before.
- **Peak:** a big query still takes the tab to about 2 GB for its few seconds: the 1 GB
  `memory_limit` plus DuckDB's working memory outside its buffers. A lower limit would refuse
  that matrix.
