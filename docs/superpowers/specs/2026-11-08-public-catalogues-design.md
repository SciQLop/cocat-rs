# Public catalogues

Decided with the user on 2026-11-08. They say this was part of their initial design.

## What

One **Public catalogues** collection per server. It's not a project anyone owns or joins: a
public bucket of catalogues.

- **Every signed-in account reads it**, in the app, in SciQLop and from Python. Anonymous
  visitors don't; the user chose "anyone signed in".
- **Nobody edits it directly except curators.** Everyone else **submits**:
  - a new catalogue: "Submit to Public catalogues" on one of their own catalogues;
  - an update: "Propose a change" on a public catalogue makes a copy in one of their projects;
    they edit it there, then "Submit as an update".
- **Curators review**: a queue of pending submissions; each shows What changed against the
  current public catalogue (events added, changed, removed); Accept or Decline, with a note.
  Accepting writes the submission into the bucket as one version, credited to the submitter,
  named "Submission N: <title>". The submitter sees the status of their submissions.
- **Curators**: the server's administrators, plus accounts an administrator marks as curator
  (admin pages).
- **AMDA**: AMDA's shared catalogues and shared time tables are imported into the bucket by a
  curator, with a script using speasy. Each keeps its AMDA id and folder; re-running updates
  instead of duplicating (ids derived from AMDA's).

## How (decisions)

- **One small public project per public catalogue**, not one project for the whole
  collection. AMDA alone shares 222 lists with 316,194 intervals (the largest 34k); one
  project holding them all would not load in a browser (11k events already cost 2 s and
  65 MB) and SciQLop would download everything to show one catalogue. Public catalogues don't
  need events shared between catalogues, which is what projects bundle for.
- A public project has a `public` flag, no owner, a namespaced id (e.g.
  `public.amda.sharedcatalog_22`, a prefix user projects can't use), and `source`, `folder`
  and `description`. Curators create them. Role resolution gives curators `editor` and every
  other signed-in account `viewer`, so everything built for projects works unchanged: sync,
  history, the read-only client, statistics, the timeline, export. They have no members,
  invitations, groups or Share dialog. They don't count against anyone's storage quota.
- The collection is an index, `GET /api/public` (Python `public_catalogues()`), not part of
  `projects()`. The app shows "Public catalogues" at the top of the project menu: an index
  view with folders and a search box; opening one shows the normal catalogue view with a
  "Public" badge.
- Submissions live in SQLite: author, kind (new / update), the target public catalogue for an
  update, the public version it was based on, title, note, status (pending / accepted /
  declined), reviewer, review note, dates, and the proposed catalogue with its events as a
  snapshot (a small Yjs update, so it reuses the copy machinery). The snapshot is taken at
  submission time; later edits in the author's project don't change it (they submit again).
- An update whose public catalogue changed since its base is marked "the public catalogue
  changed since this was submitted"; the curator can still accept (the submission wins) or
  decline asking for a fresh submission. No three-way merge in this first version.
- On screen, no developer words: Submit, Propose a change, Submissions, Accept, Decline,
  What changed.

## Later

Anonymous read access; a three-way merge for stale updates (shares the future "Apply"); email
notifications to curators and submitters; comments on submissions.
