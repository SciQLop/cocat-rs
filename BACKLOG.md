# Backlog

## Per-user and per-room quotas

Today `COCAT_ROOM_MAX_MB` and `COCAT_OWNER_MAX_MB` are server-wide: every room and every
owner gets the same allowance. Allow an override per user (owner quota) and per room
(room size), stored in the database, falling back to the environment defaults when unset.
The allowance is computed when a room opens (see the `simplify:` note in
`docs/superpowers/reports/2026-11-06-phase-4b.md`), so a changed quota applies at the next
open; say so in the panel, or reload the open room's allowance on change.

## Administration panel

The web UI has no superuser pages; administration is the CLI (`create-user`,
`add-user-to-room`, ...) and the `/users` API. A superuser-only section of the UI:
users (list, search, activate/deactivate, verify, make superuser, delete), rooms (list
with owner, size on disk, open or not, member count; change owner; delete), quotas (the
overrides above, current usage per owner and per room). Same-site guard, rate limits and
the access matrix apply like every other UI route; every action goes through the existing
lifecycle functions so open sockets are closed as they are today.

## Leads from the attacker campaign (not proven)

See `docs/superpowers/reports/2026-11-06-phase-4b.md`, "Leads not proven": export blocking
an open room, outbound queues bounded in frames not bytes, one OS thread per open room,
rate-limit key including the path.
