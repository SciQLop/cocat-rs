# Catalogue relay — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The server keeps public catalogues in step with remote providers (DONKI, CDAW LASCO
CMEs, CDAW WAVES type II bursts, AMDA), on an admin-set schedule, the provider being the
source of truth.

**Architecture:** The core gains one function that makes a catalogue hold exactly a given set
of events (adding, changing, trashing, restoring). The server gains a `relay` module: a
`Provider` trait, one module per provider, an engine that runs a provider and writes each of
its catalogues into its public room through the room manager (live for readers, one history
version per change), two tables (provider settings and runs, relayed catalogues), a
scheduler task, and an admin section. Relayed catalogues refuse every edit. The web app shows
their source and freshness.

**Tech Stack:** Rust; `reqwest` (already in the tree, rustls), `croner` (cron), `scraper`
(HTML table), `quick-xml` via `cocat_core::votable` (AMDA), `serde_json` (DONKI), `uuid` v5.

**Spec:** `docs/superpowers/specs/2026-11-10-catalogue-relay-design.md` (approved 2026-11-10,
with the calls listed in it).

## Global Constraints

- Providers are Rust code in `crates/cocat-server/src/relay/`; no plugins at run time.
- The provider is the source of truth: a run makes each relayed catalogue hold exactly the
  provider's events; removed events go to the trash and come back if republished.
- Nobody edits a relayed catalogue (curators included): no socket writes, no PATCH, no
  delete by hand, no submissions or proposals → 409 "This catalogue follows <provider>: it
  changes when <provider> does."
- Public ids `public.<provider>.<key>`; AMDA keeps `public.amda.sharedcatalog_<n>` /
  `public.amda.sharedtimeTable_<n>`, its uuid namespace `5b0f6a52-3c7e-4d61-9a0e-a3da00000001`,
  catalogue uuid = uuid5(ns, amda_id), event uuid = uuid5(ns,
  `"{amda_id}/{start iso}/{stop iso}/{occurrence}"`) exactly as `tools/amda_public.py`.
- Folders: `DONKI/<type name>`, `CDAW`, `AMDA/<AMDA folder path>`; source names "DONKI
  (NASA CCMC)", "CDAW (NASA GSFC)", "AMDA".
- A failed or empty fetch never changes a catalogue; the run is recorded as failed with its
  error.
- A catalogue the provider stops publishing stays, marked not published, no longer refreshed.
- Every provider off by default; settings in the database, no restart needed; cron in UTC;
  on start, a provider whose last success is older than one period runs once; a provider
  never runs twice at once; fetch timeout 120 s per request; User-Agent
  `cocat-relay/<version>` (no address: CDAW's firewall resets one holding a URL), no personal
  email; TLS through OpenSSL (CDAW's firewall resets rustls handshakes).
- History: a relay write is stamped with no user; history shows it as "<source name>".
- DONKI key: provider setting `api_key`, masked on the admin page; default `DEMO_KEY`.
- UI words: "From <source>, refreshed <date>", "No longer published by <source>",
  "Catalogue providers", "Refresh now".
- Process rules (scratchpad `p-rules.md`): TDD; one build/test at a time, foreground; gate
  incl. wasm clippy; explicit-path commits on main; never push. Tests never touch the
  network: providers parse fixtures saved under `crates/cocat-server/tests/fixtures/relay/`.

## Review Focus

1. A provider returning an error, a timeout or zero events: catalogue unchanged, run marked failed. → Task 2 tests.
2. A curator with a socket open on a relayed catalogue cannot write into it. → Task 2 test (socket role).
3. Upgrading cocat-dev, where the AMDA script already made `public.amda.*` rooms: the AMDA provider adopts them (same ids and uuids), the first run changes nothing for an unchanged list. → Task 6 test with a room written the script's way.
4. DONKI revising an event inside the rolling window (changed attribute) and an event older than the window: the first changes, the second is left alone (not trashed). → Task 4 test.
5. The admin sets an invalid cron line: refused with a message, the old schedule kept. → Task 3 test.

---

### Task 1: Making a catalogue hold a given set of events (core)

**Files:** create `crates/cocat-core/src/model/relay.rs` (+ `model/mod.rs` export); tests `crates/cocat-core/tests/relay.rs`.

**Interfaces (produces):**
- `pub struct Wanted { pub catalogue: String /* uuid */, pub name: String, pub events: Vec<EventRecord> }` (event uuids chosen by the caller).
- `pub struct Counts { pub added: usize, pub changed: usize, pub removed: usize, pub restored: usize }` with `fn is_empty(&self) -> bool`.
- `pub fn hold(txn: &mut TransactionMut, wanted: &Wanted, keep: impl Fn(&EventRecord) -> bool) -> Counts` — creates the catalogue if missing (or restores it from the trash), renames it to `name`, makes it list exactly `wanted.events`: missing events created and listed, differing ones rewritten (start, stop, attributes), events listed but not wanted trashed unless `keep(event)` (DONKI's "outside the window"), trashed wanted events restored.

- [ ] Failing tests: `an_empty_project_gets_the_catalogue_and_its_events`, `holding_the_same_events_again_changes_nothing` (Counts empty and `txn.encode_update_v1()` writes nothing that changes the state vector), `an_event_gone_from_the_list_goes_to_the_trash`, `an_event_back_in_the_list_comes_back_from_the_trash`, `a_corrected_event_is_rewritten`, `a_kept_event_is_not_trashed`, `the_catalogue_follows_its_name`.
- [ ] Implement with existing model functions (create/delete/restore event, list/unlist); gate; commit.

### Task 2: The relay engine and its guards (server)

**Files:** create `crates/cocat-server/src/relay/mod.rs` (trait, registry, engine), `relay/store.rs` (db), migration `0023_relay.sql`; modify `room.rs` (`Author::Server` → stamp user None), `public_api.rs`, `submissions_api.rs`, `ws.rs` (role), `history_api.rs` (label), `lib.rs`/`AppState` (providers list + `reqwest::Client`); tests `tests/relay.rs` with a fake provider.

**Interfaces (produces):**
- `pub struct Fetched { pub key: String, pub folder: String, pub name: String, pub description: String, pub url: String, pub catalogue: String /* uuid */, pub events: Vec<EventRecord> }`.
- `#[async_trait] pub trait Provider: Send + Sync { fn id(&self) -> &'static str; fn source(&self) -> &'static str; fn default_schedule(&self) -> &'static str; async fn fetch(&self, http: &reqwest::Client, settings: &serde_json::Value) -> anyhow::Result<Vec<Fetched>>; fn keep(&self, _event: &EventRecord) -> bool { false } }`.
- `AppState.providers: Arc<Vec<Arc<dyn Provider>>>` (`relay::providers()` in production; tests inject).
- `pub async fn run(state: &AppState, provider: &str) -> Result<RunReport, RelayError>`; `RunReport { catalogues: usize, counts: Counts, unpublished: usize }`.
- Tables: `relay_provider(id TEXT PK, enabled BOOL NOT NULL DEFAULT 0, schedule TEXT, settings TEXT NOT NULL DEFAULT '{}', running BOOL NOT NULL DEFAULT 0, last_run_at TEXT, last_ok_at TEXT, last_result TEXT)`; `relay_catalogue(room_id TEXT PK REFERENCES room(id) ON DELETE CASCADE, provider TEXT NOT NULL, key TEXT NOT NULL, published BOOL NOT NULL DEFAULT 1, refreshed_at TEXT)`. `room.created_by` must accept NULL for relayed rooms (check the column; migrate if needed).
- `pub async fn relayed(pool, room_id) -> Result<Option<RelayedRow>, DbError>` used by the guards.

- [ ] Failing tests (fake provider): `a_first_run_makes_the_providers_catalogues_in_its_folder`, `a_second_run_with_the_same_list_writes_nothing` (no new history version), `a_run_writes_one_version_per_changed_catalogue_authored_by_the_source`, `a_failing_fetch_keeps_the_catalogues_and_records_the_error`, `an_empty_fetch_keeps_the_catalogues_and_records_a_failure`, `a_list_the_provider_stops_publishing_stays_marked`, `a_provider_never_runs_twice_at_once`, `nobody_edits_a_relayed_catalogue` (curator PATCH 409, DELETE 409, submission/proposal 409 with the message, a curator's socket gets the viewer role so its writes are refused), `the_public_index_says_where_a_relayed_catalogue_comes_from` (`GET /api/public` rows gain `relay: {source, refreshed_at, published}` or null).
- [ ] Implement: per catalogue `read_log` → replay → `model::hold` → `encode_update_v1` → `write_as(Author::Server)` → `save_newest` named "Refreshed from <source>"; create missing public rooms (`create_public_room` with folder/source/description) and `relay_catalogue` rows; gate; commit.

### Task 3: Schedule and admin section (server)

**Files:** `relay/schedule.rs`, `ui/admin.rs` + its template, `admin.rs` (routes), `main.rs`/`lib.rs` (spawn the scheduler); tests `tests/relay_admin.rs`.

**Interfaces (produces):**
- `pub fn next_due(schedule: &str, last_ok: Option<OffsetDateTime>, now: OffsetDateTime) -> Result<OffsetDateTime, ScheduleError>` (croner, UTC; `last_ok` None → now).
- `pub fn spawn_scheduler(state: AppState)` — wakes each minute, runs due enabled providers one at a time each; on start runs those overdue by more than one period.
- Admin routes (superuser): `GET /ui/admin/providers` (table: provider, source, on/off, schedule, settings with secrets masked, last run: when, result, counts, error), `POST /ui/admin/providers/{id}` (enabled, schedule, settings), `POST /ui/admin/providers/{id}/refresh` ("Refresh now", runs in the background, redirects with a flash).

- [ ] Failing tests: `the_next_run_follows_the_cron_line_in_utc`, `an_overdue_provider_runs_on_start`, `an_invalid_cron_line_is_refused_and_the_old_one_kept`, `the_admin_page_lists_the_providers_off_by_default`, `the_key_is_masked_on_the_page`, `refresh_now_runs_the_provider`, `a_non_admin_cannot_reach_the_providers`.
- [ ] Implement; gate; commit.

### Task 4: DONKI provider

**Files:** `relay/donki.rs`; fixtures `tests/fixtures/relay/donki_<TYPE>.json` (saved from api.nasa.gov, small); tests in `relay/donki.rs` unit tests or `tests/relay_donki.rs`.

Types and catalogue names: FLR "Solar flares", CME "Coronal mass ejections", SEP "Solar energetic particles", IPS "Interplanetary shocks", GST "Geomagnetic storms", MPC "Magnetopause crossings", RBE "Radiation belt enhancements", HSS "High speed streams". Event key = the type's id field (`flrID`, `activityID`, `sepID`, `gstID`, `mpcID`, `rbeID`, `hssID`); uuid5(DONKI namespace (new, fixed in code), key). Start/stop: FLR `beginTime`/`endTime` (stop = peak or begin when missing), others `startTime`/`eventTime` with stop = start. Attributes: the scalar fields as `#`-free names, instruments and linked event ids joined. Window: first run (no `last_ok_at`) from 2010-01-01 in yearly requests; later runs the last 90 days; `keep` = event start older than the window.

- [ ] Failing tests on fixtures: `donki_flares_become_events_keyed_by_their_id`, `a_flare_without_an_end_stops_at_its_peak`, `each_type_lands_in_its_catalogue_under_donki`, `events_before_the_window_are_kept` (Review Focus 4), `the_key_comes_from_the_settings`.
- [ ] Implement; gate; commit.

### Task 5: CDAW providers (LASCO CMEs, WAVES type II)

**Files:** `relay/cdaw.rs`; fixtures `cdaw_univ_all_head.txt` (header + ~30 lines incl. remarks and missing values), `cdaw_waves_type2.html` (trimmed table); tests alongside.

LASCO: `https://cdaw.gsfc.nasa.gov/CME_list/UNIVERSAL_ver2/text_ver/univ_all.txt`, key = `"<date> <time>"` of first C2 appearance, stop = start (point event), attributes `central_pa_deg`, `width_deg`, `linear_speed_km_s`, `speed_initial_km_s`, `speed_final_km_s`, `speed_20rs_km_s`, `accel_m_s2`, `mass_g`, `kinetic_energy_erg`, `mpa_deg`, `remarks`; values like `----` or `Halo` handled (Halo → width 360, central PA absent). Type II: `https://cdaw.gsfc.nasa.gov/CME_list/radio/waves_type2.html`, key = start date+time, stop from end date/time, attributes `freq_start_khz`, `freq_end_khz`, `flare_location`, `noaa_ar`, `importance`, `cme_date_time`, `cme_cpa_deg`, `cme_width_deg`, `cme_speed_km_s`. One provider id `cdaw` with two catalogues in folder `CDAW`: "SOHO LASCO CMEs", "WAVES type II bursts".

- [ ] Failing tests: `a_lasco_line_becomes_an_event_with_its_measures`, `a_halo_cme_is_360_degrees_wide`, `missing_values_are_left_out`, `a_type_ii_row_spans_its_burst`, `a_burst_ending_the_next_day_ends_the_next_day`.
- [ ] Implement with `scraper`; gate; commit.

### Task 6: AMDA provider, replacing the script

**Files:** `relay/amda.rs`; fixtures `amda_catalogs.xml`, `amda_timetables.xml` (trimmed trees), one catalogue and one time table VOTable; remove `tools/amda_public.py`, `tools/test_amda_public.py` and their CI step; docs/deploy.md section updated.

Fetch: `GET https://amda.irap.omp.eu/php/rest/getCatalogsList.php` and `getTimeTablesList.php` answer a URL of an XML tree; each `catalog`/`timeTable` element (id, name, description) under its `folder` path; `getCatalog.php?catID=<id>` / `getTimeTable.php?ttID=<id>` answer a URL of a VOTable read with `cocat_core::votable::import`. Folder `AMDA/<folders>`; ids, uuids and attribute naming as the script (Global Constraints); description cut at 2000 characters with "…".

- [ ] Failing tests: `the_tree_gives_each_list_its_folder`, `a_list_keeps_the_scripts_ids` (catalogue and event uuids equal to values computed with the script's formula, written as literals in the test), `the_nth_copy_of_an_interval_gets_occurrence_n`, `a_room_the_script_made_is_adopted_unchanged` (Review Focus 3: build the room the script's way, run the provider on the same list, no version written).
- [ ] Implement; gate; commit.

### Task 7: Show where a relayed catalogue comes from (web, controller)

**Files:** `crates/cocat-web/src/public.rs` (row `relay`), `views/public.rs` (index line), the catalogue heading (public catalogue page) and `views/actions.rs` (no Submit/Propose/update for relayed; Copy stays); style.

- [ ] Native tests: the relay line text (`relay_text(source, refreshed, published)`).
- [ ] Browser: a fake-provider-fed server (test binary flag or seeded tables) shows "From DONKI (NASA CCMC), refreshed 2026-11-10" and the source link; a not-published one says so; a curator sees no edit controls; light + dark.
- [ ] Gate; commit.

### Task 8: Docs (controller)

- [ ] Help (Public catalogues): relayed catalogues, their sources, why they cannot be edited, copying them. README: providers, admin section, settings, schedule, User-Agent. docs/deploy.md: turning providers on, DONKI key. Help test first; gate; commit.
