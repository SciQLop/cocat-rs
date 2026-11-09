# Home page: what needs your attention

## Why

A person in several projects has no single place that says what needs them. Invitations
sit in their mailbox. Submissions to the public catalogues have their own view. What others
changed in a project shows only once they open that project. The home page gathers both
kinds of attention in one place: what is waiting on you, and what changed.

## What it shows

1. **Waiting for you** (hidden when empty):
   - Invitations to a project addressed to the visitor's email, still valid (not revoked,
     not expired, uses left). Each one: project name, role, who invited, with **Accept** and
     **Decline**. Accept joins the project and opens it; Decline revokes that invitation.
   - For curators: submissions to the public catalogues still pending, with a link to the
     existing review view.
   - The visitor's own submissions: pending ones, and the ones accepted or declined in the
     last 30 days, with the curator's note.
2. **What changed**: one card per project the visitor belongs to, most recently changed first.
   - The changes other people made since the visitor last opened that project (their own
     changes are not news). At most 5 versions per project, then "and N more"; the card
     links to the project's history.
   - Each line: author (in their colour), what they did in plain words (counts of events
     added/changed/removed, the catalogues touched, rules changed, a restore), and when.
   - A project with nothing new shows as one compact line: name, role, last change.
   - Never opened: changes of the last 30 days count as new. Nothing older than 30 days
     counts as new either way.

## How it is reached

- A hash route `#home` in the web app. The cocat logo goes there; the avatar menu gets a
  **Home** entry first. The app still opens on the last project as today (my call: no
  change to where people land).
- The public index's header and the project header both show the logo, so Home is one click
  from anywhere.

## Server

- Migration `0020_room_seen.sql`: `room_seen(user_id, room_id, seen_ms)`, primary key
  `(user_id, room_id)`, both cascading on delete. Touched when a member's socket is accepted
  for the room (the web app, Python and SciQLop all count as "opened it").
- `GET /api/me/home` (cookie or token): one JSON answer with `invitations`, `reviews`
  (curators only, else empty), `submissions`, and `projects` (each: id, name, role,
  `last_change_ms`, `seen_ms`, `changes` (up to 5 version summaries in the history API's
  shape, with author email and colour), `more`).
- `POST /api/invitations/{id}/accept` and `POST /api/invitations/{id}/decline`: only for an
  invitation addressed to the caller's verified email; anything else is 404. Accept goes
  through the same membership code as the token path.
- Versions per room are cached keyed by the room's record count (the upgrade path the
  `simplify:` note in `history_api.rs` names), so the home page does not replay every
  project's whole log on each load. The history endpoint uses the same cache.

## Non-goals

- No notifications by email or push, no unread badges elsewhere in the app.
- No Python or SciQLop API for it now (add `Client.home()` when a script asks for it).
- No charts.
