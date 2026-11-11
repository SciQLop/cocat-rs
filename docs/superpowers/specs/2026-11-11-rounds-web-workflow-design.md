# Selection rounds on the web: each person does their part from the page

Builds on `2026-11-11-selection-rounds-design.md` (rounds, proposals, votes, budget), which is
implemented. This document changes votes into verdicts and makes the web page useful.

## Why

The Rounds entry appears in every project that has a round. Today the page only shows a round:
regular users cannot tell what a round is, and cannot take part without Python. The user
(2026-11-11): making it accessible and useful for their workflow "is the only legitimate reason
to expose the UI on all the projects".

A third workflow joins downlink selection and labelling: **curating ML detection output**. A
pipeline saves its detections with the Python API; people review them; a curated reference
catalogue comes out, and the pipeline reads the verdicts back (precision, retraining).

## What it is (decided by the user, 2026-11-11)

1. **The page explains itself.** One sentence on what a round is at the top of the Rounds page;
   per round a "Your part" box saying what the visitor has to do; the final catalogue shows
   "Decided in round *name*" linking to the round; a "Selection rounds" section on the Help page.
2. **Starting a round is one form with three presets**: *Review detections*, *Selective
   downlink*, *Labelling*. Presets only pre-fill the same round settings.
3. **Review candidates come from a catalogue already in cocat, or from an imported file** ("it
   could be both, just UX ergonomy"). A file goes through the existing import first; the round
   then takes its candidates from that catalogue either way.
4. **Proposing** opens the visitor's proposal in the normal catalogue view (table, timeline,
   event editor, import).
5. **Reviewing**: a vote becomes a **verdict per event**: `accept`, `reject` or `unsure`; no
   verdict means "not reviewed yet". One review table with keyboard shortcuts and progress.
6. **Deciding**: a "Build the final catalogue" view: candidates sorted by verdicts, the budget
   gauge live, a one-click default keeping what a majority accepted, then the decider adjusts and
   confirms. The page writes the final catalogue (`proposed_by`, verdict counts) and decides.
7. **SciQLop keeps** looking at the data and merging overlapping intervals from different people.
   On the web, overlapping proposals stay separate rows.

## Goals

- A member who has never heard of rounds understands one from the page in a few seconds and
  knows what is asked of them.
- Every round step a regular user takes is doable on the web: start a round, propose, review,
  decide. Python keeps every step too.
- Review scales to thousands of candidates: one key per verdict, the next unreviewed one comes
  up by itself, progress always visible.
- A pipeline can run the loop from Python: save output → start a review round on it → read
  per-event verdicts back, matched to its own event ids.
- Labelling and downlink rounds keep working: "support" is `accept`.

## Non-goals

- Plotting data on the web; merging overlapping intervals into candidates.
- Automatic decision rules beyond the one-click majority default.
- Reviewing in another project than the round's.

## Anti-goals

- A new user sees "Rounds" and cannot tell what it is or whether it concerns them.
- A reviewer loses their place in a long review (reload, another tab) or cannot tell what is left.
- Candidates changing under reviewers: a pipeline overwriting its catalogue mid-review must not
  move verdicts onto other detections.
- Any regression of the blind rules: verdicts follow the same visibility as votes did.

## Shape (decided parts)

### Kinds

A round gets a `kind`: `review`, `downlink` or `labelling` (stored; drives the wording and the
presets, never the rules). Existing rounds become `labelling`, or `downlink` when they have a
budget.

| Preset | Visibility | Budget | Candidates | Proposers by default | Wording of "Your part" |
|---|---|---|---|---|---|
| Review detections | open | none | required | none (the candidates are the proposal) | "Review N detections (k done)" |
| Selective downlink | open | form shown | optional | owners and editors | "Propose your intervals" |
| Labelling | blind | none | optional | owners and editors | "Label on your own until …" |

### Candidates

- At creation the round copies the chosen catalogue's live events (trash excluded) into a
  **candidates proposal**, authored by the round's creator, **keeping each event's uuid**, and
  records the copy hop (existing copy-chain code) naming the source project and catalogue.
- Source: any catalogue the creator can read (this project, another project, a public one).
- From a file: the existing import writes a catalogue into the round's project first, then the
  same copy happens.
- The candidates proposal is frozen from the start (nobody edits it); reviewers who want to add a
  missed event do so in their own proposal, if they are proposers.

### Verdicts

- Stored per (round, reviewer, event): `accept | reject | unsure`; absent = not reviewed.
  Replaces `round_vote` (migration: existing votes become `accept`).
- Written only while `voting` (renamed on the page "reviewing" for review rounds), by voters,
  one event at a time (`PUT …/verdicts/{event}`) and in bulk.
- Read rules unchanged from votes (blind: the reviewer, deciders, owners until decided).
- `GET …/verdicts` → per event: counts of accept/reject/unsure and, when readable, who said what.

### The web page

- Rounds page: intro sentence; rounds grouped "Needs you" / "In progress" / "Finished"; each with
  its kind and state in words.
- Round page: "Your part" box first; then candidates/proposals, verdict progress, budget, log.
- New round form (owners and editors): preset, name, people per role, deadline, visibility,
  budget form (attribute, a rate per value, cap; units picked from a short list of VOUnits
  spellings), candidates picker (catalogue, or "Import a file…").
- Review table: one row per candidate the visitor may read; columns start, stop, duration,
  `confidence`, the round's budget attribute, other attributes; keys `a` accept, `r` reject,
  `u` unsure, `j`/`k` next/previous, the next unreviewed row selected after each verdict; filter
  "not reviewed / all"; counter "120 of 900 reviewed".
- Decide view (deciders, from `voting`): candidates with verdict counts, sorted by accepts;
  "Keep what a majority accepted" ticks rows; ticking updates the budget gauge; "Decide" writes
  the final catalogue into the project (events copied, `proposed_by`, `accepts`, `rejects`,
  `unsure` attributes) and moves the round to decided.
- Final catalogue marker: from its `round` attribute.

### Python

- `create_round(..., kind=, candidates=catalogue | (project, catalogue uuid), proposers=,
  voters=, deciders=)`.
- `verdict(round, event, "accept")`, `verdicts(round)` (the GET above), `vote` stays as
  "accept these" for compatibility.

## Calls (mine — check them)

- **Copy, not reference**: the candidates are a snapshot, so verdicts never drift; the copy
  chain says where they came from.
- **The decider's page writes the final catalogue client-side** (as the spec already trusts the
  decider); the server keeps stamping the `round` attribute.
- **No new role names**: "voter" shows as "reviewer" in review rounds.

### Built to grow into review cycles (the user, 2026-11-11)

Later workflows reuse rounds: public submissions, and pull-request-like cycles that go back and
forth — submit → review → reject or ask for changes → submit again → review… This work does not
build them, but shapes rounds so they need no rewrite:

- **The workflow is data.** Each kind has a table of allowed moves: `(from, to, who)` with `who`
  one of decider, owner, proposer, deadline. The rules read the table; no state order is coded
  anywhere else. Today's tables reproduce today's behaviour (forward by a decider, back by an
  owner, `proposing → voting` by the deadline). A PR-like kind is then a new table, e.g. a
  decider moving `voting → proposing` with "changes requested", and a proposer moving
  `proposing → voting` with "submitted again".
- **A move can carry a note**, stored in the round's log ("Please split the long event", "Fixed
  the stop times"). The page shows it with the move. This is the review conversation.
- **Each return to `proposing` starts a new revision** (a counter on the round). Verdicts record
  the revision they were given in; the page shows verdicts from an earlier revision as "from the
  previous review", and counts only the current revision's verdicts as progress.
- **Where the decided catalogue goes is one function** (today: into the round's own project).
  A public submission later makes it the public project, and a "changes" submission makes it a
  diff applied there; both are new cases of that one function.

### Calls made after the builder check (mine — check them)

- **Ids.** Candidates keep the source event uuids (verdicts are keyed by them, so a pipeline
  matches verdicts to its own output). Final events get **fresh uuids** — the source events may
  live in the same project — and carry a `candidate` attribute with the candidate's uuid.
- **The candidates proposal** is created by the server at round creation (a `candidates` flag on
  its `round_proposal` row; author = the creator, who may also have an ordinary proposal).
  Nobody writes it, in any state. It is readable by every project member once the round leaves
  `proposing`, and by the deciders and owners before, whatever the visibility (it is nobody's
  private work). The copy reuses the existing copy code and refuses a source the creator cannot
  read.
- **A review round without proposers starts in `voting`** (logged "state: voting" by its
  creator); with proposers it starts in `proposing` as usual.
- **Deciding from the page is retryable**: the final catalogue's uuid is derived from the round
  id (uuid v5), so a second attempt after a failure rewrites the same catalogue instead of
  leaving a stray one.
- **Verdicts.** `GET …/verdicts` names who said what when readable (as votes did). The bulk PUT
  merges; the old `vote(events)` call sets `accept` on those events and clears the voter's other
  `accept`s, leaving their `reject`/`unsure` alone.
- **`kind` is a plain stored field**, changeable by PATCH; any kind may carry a budget.
- **A file imported from the round form stays as a normal catalogue** of the project even if the
  form is abandoned.
- **Candidates reach the page through the proposal's document socket** (read-only for
  reviewers), as STIX-sized catalogues already do.
- **Moving back** keeps verdicts; verdicts on events no longer among the candidates are not
  counted, and the page says how many were set aside.

## Left to the builder

- Table names, routes, the shape of the bulk verdict body.
- Layout and copy of each view within the existing design.
- How the review table keeps the visitor's place across reloads (the server's verdicts are the
  truth; the selected row may live in the URL).
