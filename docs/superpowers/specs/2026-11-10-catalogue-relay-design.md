# Catalogue relay: public catalogues kept in step with remote providers

## Why

Event lists live on many servers (NASA CCMC, CDAW, AMDA, …), each with its own format and
schedule. Researchers want them in one place, next to their own catalogues, and current:
when a provider adds or corrects events, cocat should follow without anyone re-importing by
hand. Today one Python curator script (`tools/amda_public.py`) imports AMDA's shared lists;
nothing keeps them fresh and nothing covers other providers.

## What it is (decided by the user, 2026-11-10)

- **Providers are Rust code inside the server**, one module per provider, written by the
  maintainer. Nothing is installed or uploaded at run time.
- **The provider is the source of truth.** A refresh makes each relayed public catalogue
  hold exactly what the source says now: new events appear, removed ones go, corrected ones
  change. No curator review.
- **The administrator** enables or disables each provider and sets its refresh schedule
  (a cron line). The settings live in the database; changing them needs no restart.
- **One folder per provider** in the public catalogues: `DONKI/…`, `CDAW/…`, `AMDA/…`; a
  provider's own grouping becomes sub-folders.
- First version: the simplest and most valuable sources, preferring lists AMDA does not
  already have.

## Goals

- A relayed catalogue updates on schedule and on demand ("Refresh now" for the admin).
- Each refresh that changes something is one history version, so "What changed" shows what
  the provider changed, and when.
- Each relayed catalogue names its source (provider, URL, licence/acknowledgement text) and
  shows when it was last refreshed successfully.
- Event identity is stable across refreshes: an event keeps its uuid when the provider keeps
  it, so links, copies and people's catalogues that list it stay valid. The uuid derives
  from the provider's own event id when it has one (uuid5 of provider + id), else from the
  event's defining fields; the AMDA script's rule is the model.
- A failed or empty fetch never empties a catalogue: the last good copy stays, the run is
  recorded as failed with its error, the admin page shows it.

## Non-goals / anti-goals

- No plugin API for third parties, no Python providers, no uploads of code.
- No editing of relayed catalogues: no direct edits, no proposals or submissions (the next
  refresh would undo them). People copy one into a project to work on it; the copy chain
  records where it came from.
- No merging of overlapping lists from different providers: each list stays as published
  ("there are no definitive event lists" — HELIO).
- Failure: a provider hammering a remote server. Refreshes are at most daily by default,
  one at a time per provider, with a timeout, and identify cocat in their User-Agent
  (generic name, no personal email).

## First providers

Checked 2026-11-10 against AMDA's lists (AMDA already has Richardson & Cane, Jian's
ICME/SIR, several shock lists and SERPENTINE lists):

1. **DONKI (NASA CCMC)** — `https://api.nasa.gov/DONKI/<type>?startDate&endDate&api_key`;
   JSON. Types: FLR (flares), CME, SEP, IPS (interplanetary shocks), GST (geomagnetic
   storms), MPC (magnetopause crossings), RBE (radiation belt enhancements), HSS (high
   speed streams). One public catalogue per type under `DONKI/`. Event id = `activityID`
   (`flrID`, `gstID`, … per type). The admin stores a NASA API key (DEMO_KEY is
   rate-limited). The direct CCMC endpoint timed out from LPP; api.nasa.gov answers.
   Full history fetched in yearly windows on the first run, then a rolling window (e.g.
   the last 90 days) on each refresh, since DONKI revises recent events.
2. **SOHO/LASCO CME catalogue (CDAW)** —
   `https://cdaw.gsfc.nasa.gov/CME_list/UNIVERSAL_ver2/text_ver/univ_all.txt`, fixed-width
   text (~5 MB, updated weekly). One catalogue `CDAW/SOHO LASCO CMEs`; event = the CME's
   first C2 appearance (start; stop = start, or a fixed duration — builder's call, stated
   in the source note); attributes: central PA, width, linear speed, second-order speeds,
   acceleration, mass, kinetic energy, MPA, remarks.
3. **STEREO and Wind/WAVES type II bursts (CDAW)** —
   `https://cdaw.gsfc.nasa.gov/CME_list/radio/waves_type2.html`, one HTML table (use an
   established HTML parser crate, e.g. `scraper`). One catalogue `CDAW/WAVES type II
   bursts`; start/stop and frequency range per burst, linked CME fields as attributes.
4. **AMDA shared catalogues** — ported from `tools/amda_public.py` (AMDA web services
   `getCatalogsList` / `getTimeTablesList` → VOTable, which cocat already reads), same ids
   and folders so existing public catalogues continue; the Python script is retired.

Later, not in the first version: Helsinki heliospheric shock database (no machine download
today; ask its maintainers), Wind ICME list (HTML), HEK solar events.

## Shape (decided parts, and the builder's)

- A provider implements one small trait: its id and display name, its default schedule,
  and `fetch(&Http, &Settings) -> Result<Vec<RelayedCatalogue>>` where a relayed catalogue
  is folder, name, description, source note and its events (with stable ids). The relay
  engine (not the provider) diffs against the stored public catalogue and writes one
  update.
- Schedule: cron syntax via an established crate (e.g. `cron` or `croner`); one scheduler
  task in the server; a provider run is skipped if the previous one is still going.
- Admin page (existing `/ui/admin`): a "Catalogue providers" table — on/off, schedule,
  settings (DONKI key), last run (when, result, counts +new/~changed/−removed, error),
  "Refresh now".
- Public index and catalogue page show "From <provider>, refreshed <date>" and the source
  link; relayed catalogues offer Copy, not Submit/Propose.
- Default: every provider off until the admin turns it on (a fresh install calls no
  remote server by itself).

## Calls made after the builder check (mine, not the user's — check them)

- **Removed events go to the trash** (as the AMDA script does today) and come back if the
  provider publishes them again; the trash is not shown in the public index or counted.
- **A table marks relayed catalogues** (`relay_catalogue`: public id → provider, its key);
  ids are `public.<provider>.<key>` (`public.donki.flr`, `public.cdaw.lasco`), AMDA keeps
  `public.amda.sharedcatalog_<n>` and its uuid namespace, so today's AMDA catalogues are
  adopted, not duplicated.
- **Nobody edits a relayed catalogue**, curators included: no edits, no rename or delete by
  hand, no submissions or proposals. The provider's list of catalogues drives creation; a
  catalogue the provider stops publishing stays, marked "No longer published by
  <provider>", and is no longer refreshed. Turning a provider off keeps its catalogues.
- **AMDA is fetched over plain HTTP** (`/php/rest/getCatalogsList.php`,
  `getTimeTablesList.php` answer, without sign-in, the URL of an XML tree; each list is a
  VOTable read by `cocat_core::votable`). No Python, no speasy.
- **The engine writes through the room manager**, the same path as accepting a submission,
  so readers on a socket see the refresh live; history names the author `relay:<provider>`.
- **Event keys without provider ids:** LASCO uses its first-appearance date and time (the
  catalogue's own key); type II bursts use their start date and time. A provider changing
  that key shows as one event removed and one added — accepted.
- **The DONKI key** is a provider setting in the database, masked on the admin page, used
  from the next run.
- **Schedule:** one server process; cron in UTC; on start, a provider whose last success is
  older than one period runs once.

## Left to the builder

- Exact attribute names per provider (snake_case, units in the name, e.g. `speed_km_s`).
- Duration of point-like events (CME first appearance, DONKI flare without end).
- How much DONKI history to backfill and the rolling window size.
- Whether runs are kept in a table (`relay_run`) or only the last one per provider.
