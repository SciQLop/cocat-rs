# Administration dashboard — design

## Why

Today a superuser administers cocat through the CLI inside the container and the `/users`
API in Swagger UI. A lab running its own server needs to see at a glance how the server is
doing and act on users and rooms from the browser, with no Grafana and no shell. It is
also where per-user and per-room storage quotas get set (backlog).

## Goals

- One superuser-only section of the web UI, `/ui/admin`, in the same look as the rest.
- **Overview** (`/ui/admin`): the state of the server in one screen.
  - Users: total, active, unverified, superusers, signed up in the last 7 days.
  - Rooms: total, open now, connected clients now.
  - Storage: total size of the room files; the 10 largest rooms with owner, size and the
    share of their limit used; owners above 80 % of their quota.
  - Server: version, uptime, resident memory (`/proc/self/statm`), the configured limits.
- **Users** (`/ui/admin/users`): a table with an email search. Columns: email, signed up,
  verified, active, superuser, rooms owned, storage used / quota. Actions: activate or
  deactivate, mark verified, grant or remove superuser, set or clear a storage quota,
  delete (with confirmation).
- **Rooms** (`/ui/admin/rooms`): a table sorted by size. Columns: name and id, owner, size /
  limit, members, open, connected clients. Actions: change owner, set or clear a size
  limit, delete (with confirmation).
- **Quotas:** a per-user quota (total of the rooms they own) and a per-room size limit,
  each optional, in MB, overriding `COCAT_OWNER_MAX_MB` / `COCAT_ROOM_MAX_MB`. Stored in the
  database (migration 0007, two nullable columns). The room's allowance reads the override
  when it opens, as it reads the environment today, so a change applies at the next open;
  the page says so.
- An "Admin" link in the header, shown to superusers only.

## Non-goals

- Charts over time, storage growth, request latency: that is the Grafana metrics export
  (backlog), not this page.
- Audit log, impersonation, bulk actions, editing room contents.
- An admin API beyond what `/users` already offers (the pages post forms, like the rest of
  the UI).

## Constraints

- Every admin route checks `is_superuser` on the server. Everyone else, signed in or not,
  gets 404, as for a room they cannot see: the section does not reveal it exists. Rows go
  into the access matrix.
- Actions reuse the existing lifecycle functions, so their side effects stay the same:
  deactivating or deleting a user ends their sessions and closes their sockets, deleting
  a room closes its sockets and removes its file, changing an owner goes through the
  ownership transfer.
- A superuser cannot deactivate, delete or demote themselves (keeps at least one admin).
- Same-site guard, rate limits and the confirmation script apply as on every UI form.
- Counting connected clients needs a small read-only accessor on the room manager (open
  rooms are already counted); nothing else about the room threads changes.
- File sizes come from the files on disk; the overview reads them per request.
  `simplify:` one `stat` per room per page load; fine for hundreds of rooms, cache it if a
  deployment reaches tens of thousands.

## Left to the builder

Layout details within the existing stylesheet, table sorting beyond "largest first",
wording of the empty states.
