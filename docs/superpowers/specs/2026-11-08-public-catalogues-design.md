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

- The bucket is a project with a fixed id, `public`, created at startup if missing, with no
  owner. Role resolution gives every signed-in account `viewer` on it and curators `editor`.
  So everything built for projects works on it unchanged: sync, history, the read-only client,
  statistics, the timeline, export. It can't be renamed, deleted, shared or left; it has no
  Share dialog. It doesn't count against anyone's storage quota.
- `projects()` / `GET /api/rooms` list it for everyone, with `public: true`. The app shows it
  as its own entry, "Public catalogues", above the user's projects.
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
