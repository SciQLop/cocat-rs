# cocat-rs

<img src="docs/banner.png" alt="CoCat, the cosmic collaboration cat" width="500">

The Rust server for cocat: projects over the Yjs sync protocol, the project lifecycle, and the
seeding CLI. The Python `cocat` keeps running until this server is feature-complete; the
wire format and the `.y` project files are shared with it.

A project holds catalogues and their events. Everyone in a project sees all of it, with the
role they were given; an event can belong to several catalogues of one project; history
covers the whole project. The API, the websocket path and the CLI call a project a
*room*. The Python package says project.

## Build

```sh
cargo build --release -p cocat-server
```

The binary is `target/release/cocat-server`.

## Container

```sh
podman build -t cocat-server-rs -f docker/Dockerfile .
podman run -d --name cocat -p 6570:6570 \
    -v cocat-storage:/storage -v cocat-db:/db -v cocat-log:/log \
    cocat-server-rs
```

The image runs as the `cocat` user and keeps the Python image's contract:
`STORAGE_PATH=/storage` for the project files, `DB_PATH=/db` for `cocat-rs.db`, `LOG_PATH=/log`,
`PORT` (build argument, 6570 by default), and the `COCAT_PROXY_PREFIX` and
`COCAT_TRUSTED_PROXIES=1` it sets for a deployment behind one proxy. Every mount is declared
as a volume. The entry point `exec`s the server, so `podman stop` sends it `SIGTERM` and the
open projects are flushed before it exits.

Three wrappers take the same positional arguments as the Python image:

```sh
podman exec cocat create-user you@example.org secret
podman exec cocat add-user-to-room them@example.org my-project
podman exec cocat remove-user-from-room them@example.org my-project
```

Each release publishes the image as `ghcr.io/sciqlop/cocat-server:<version>` and `:latest`.
`docker/build.sh <image>[:tag] [--push]` builds it from a checkout, with podman or docker.

Running a server (reverse proxy, mail, metrics, backups, moving from a Python server) is
documented step by step in [docs/deploy.md](docs/deploy.md).

## Seeding

```sh
cocat-server create-user --email you@example.org --password 'plum orbit lantern cello' [--is-superuser]
cocat-server set-superuser --email you@example.org [--revoke]
cocat-server set-curator --email them@example.org [--revoke]
cocat-server create-room --owner you@example.org [--room-id my-project] [--name "My project"]
cocat-server add-user-to-room --email them@example.org --room_id my-project [--role editor]
cocat-server remove-user-from-room --email them@example.org --room_id my-project
cocat-server set-room-owner --email them@example.org --room_id my-project
```

Every command takes `--db_path` (default `./test.db`). `create-user` refuses a weak
password the same way the sign-up page does (see [Passwords](#passwords)). Project ids match
`[A-Za-z0-9_-]{1,64}`. `add-user-to-room` takes `--role owner|editor|viewer` and never
lowers an existing role; the owner role is not assignable that way, `set-room-owner` is.

## Serving

```sh
cocat-server serve [--host localhost] [--port 8000] [--update_dir ./updates] [--db_path ./test.db]
```

It prints `listening on <addr>` (`--port 0` lets the kernel pick) and shuts down
gracefully on SIGINT or SIGTERM, after flushing every open project. Project files live in
`--update_dir` as `<room_id>.y`.

### Environment

| Variable | Meaning |
|---|---|
| `COCAT_SECRET` | Secret that signs the mailed links (verification, password reset) and the provider sign-in cookie. Session cookies carry an opaque token the server stores instead of a signature. When it is unset or empty, the server uses `<db_path>.secret`, creating it (mode 0600) on first start. |
| `COCAT_ROOM_MEMORY_BUDGET` | Ceiling in **megabytes** for loaded projects. Unset or empty means no limit; a value that is not a positive integer stops the server at startup and names the variable. A project is charged sixteen times its `.y` file size (a measured 37.8 MB project is ~566 MB resident), so the budget estimates the memory a run can reach rather than measuring it. An open that would exceed the budget is refused; the project stays on disk. |
| `COCAT_ROOM_MAX_MB` | Largest `.y` file one project may reach, 1024 by default. Unset or empty means the default; a value that is not a positive integer stops the server at startup and names the variable. An update that would take the file past the limit is refused: nothing is applied, appended or broadcast, and its sender is closed with 4507 while the other clients stay. |
| `COCAT_OWNER_MAX_MB` | Total `.y` bytes the projects one account owns may reach, 5120 by default, measured when a project opens. Editors' writes count against the owner. Same parsing rules as `COCAT_ROOM_MAX_MB`. |
| `COCAT_METRICS_BIND` | Address for the Prometheus endpoint, such as `127.0.0.1:9187`. Unset or empty exports nothing; a value that is not an address stops the server at startup and names the variable. The endpoint is a separate listener, never the public port. |
| `COCAT_METRICS_TOKEN` | Bearer token for the metrics endpoint. Unset or empty leaves `/metrics` open; anything shorter than 16 characters stops the server at startup and names the variable. The token is never logged and never appears in a label. |
| `COCAT_TRUSTED_PROXIES` | How many proxies stand in front of the server. Unset or empty means zero: the socket address is the client and `X-Forwarded-For` is ignored, because any client can write that header. With `N` proxies the client is the `N`-th hop from the right of `X-Forwarded-For`, believed only when the header carries at least `N` hops. A value that is not a non-negative integer stops the server at startup and names the variable. |
| `COCAT_PUBLIC_URL` | The address users reach this deployment at, scheme and host only (`https://cocat.example.org`; a path, query or fragment is dropped). Every mailed link is built from it plus `COCAT_PROXY_PREFIX`, never from the request's `Host` header, which any client can spoof. A mailer configured without it stops the server at startup and names the variable. |
| `COCAT_PROXY_PREFIX` | Path prefix the reverse proxy adds in front of the application (`/cocat`), used to build links that survive the proxy. Unset or blank means none. |
| `COCAT_SMTP_HOST` | SMTP relay. Unset or empty means the server sends no mail: registration and password reset then have nothing to deliver, and invitations return their token in the response instead of mailing it. |
| `COCAT_MAIL_FROM` | Envelope sender, required with `COCAT_SMTP_HOST` and with `COCAT_MAIL_DIR`. A value that is not a single address, or that carries a line break, stops the server at startup. |
| `COCAT_SMTP_PORT` | Relay port, 587 by default; a value that is not a port number stops the server at startup. |
| `COCAT_SMTP_USERNAME` / `COCAT_SMTP_PASSWORD` | Relay credentials. Give both or neither: a username without a password stops the server at startup, and a password without a username is refused rather than ignored. |
| `COCAT_SMTP_STARTTLS` | Whether to upgrade the connection before sending, on by default. `1`/`true`/`yes`/`on` and `0`/`false`/`no`/`off`, case-insensitive and trimmed; any other value stops the server at startup. |
| `COCAT_MAIL_DIR` | Write one `.eml` file per message instead of talking to a relay, for tests and demos. The directory must already exist (a send that cannot write names the path). Mutually exclusive with `COCAT_SMTP_HOST`: setting both stops the server at startup. |
| `COCAT_OAUTH_PROVIDERS` | JSON list of OpenID Connect providers, empty or unset for none (see [Provider sign-in](#provider-sign-in)). Every provider needs `COCAT_PUBLIC_URL`; a value that is not a JSON list, or a provider whose name is not `^[a-z0-9_-]{1,32}$`, stops the server at startup. A provider whose discovery document cannot be read at startup is skipped with a warning and the server still starts. |

### Passwords

Every password that is set (sign-up, reset, change, `create-user`) has at least 8
characters and a [zxcvbn](https://github.com/shssoichiro/zxcvbn-rs) score of 3 or more,
with the account's address counted as known to an attacker. No composition rules. A refusal
says why in plain words, with zxcvbn's suggestion (the API answers 400 with it as `detail`).
Passwords stored before the rule keep working: it is checked when a password is set, never
at login.

### Tokens and browser sign-in

Apps sign in with a **token**: a named session ("Python on lpp-jupyter") sent as
`Authorization: Bearer <token>`, accepted wherever the session cookie is (API, websocket,
history, public catalogues, submissions). A token lasts until revoked, or six months unused;
using it moves that date, at most once an hour. A password change ends browser sessions and
keeps tokens. The account page lists them under "Signed-in apps", each with Revoke, and so
does the API:

| Route | Answer |
|---|---|
| `GET /api/tokens` | `[{"id", "name", "created_at", "last_used_at"}]`, newest first; `id` is not the token |
| `DELETE /api/tokens/{id}` | 204, or 404 for no such token of the caller's |
| `DELETE /api/tokens/current` | 204: the token (or cookie) this call carries is ended (an app's logout) |

Revoking a token closes the websockets it opened (1008) at once.

An app gets a token through the browser, without a password:

1. `POST /api/sign-in-requests` with `{"name": "Python on <host>"}`, no session needed,
   answers 201 `{"code": "K7QM-2XRD", "url": "<base>/ui/connect?code=K7QM-2XRD", "poll":
   "<secret>", "expires_in": 600, "interval": 2}`. `<base>` is `COCAT_PUBLIC_URL` with the
   proxy prefix, or the `Host` the app called. Limited like a login: ten a minute per client.
2. The app shows the link and the code. The person opens the link, signs in (password or
   provider), checks the code and clicks Allow or Deny.
3. The app polls `POST /api/sign-in-requests/poll` with `{"poll": "<secret>"}` every
   `interval` seconds: `{"status": "pending"}`, `"denied"`, `"expired"` (also for an
   unknown or spent request), or `{"status": "approved", "token": "..."}`, once: the request
   is gone afterwards. A poll within a second of the previous one answers 429
   `{"detail": "slow_down"}`.

The server keeps only hashes of tokens and poll secrets.

### Login rate limit

`POST /auth/jwt/login` takes ten calls per minute per client (the socket address, or the
trusted hop above); the eleventh answers 429 with a plain body until a minute has passed
since the first call of the window. Every route keeps its own budget, so a flood of login
attempts cannot lock a client out of the rest of the API. The table lives in memory, holds
at most 10 000 clients (the oldest window is evicted first) and is empty again after a
restart.

### Close codes

| Code | Meaning |
|---|---|
| 1008 | session ended: the token the socket was opened with was revoked, its password was reset, the account was deactivated or deleted, or it logged out. Sign in again |
| 4403 | access changed: the member was removed or left, their role changed (directly, through a group, or as a curator), ownership moved, or the project was deleted. The session is still good: reconnect at once, and the handshake names the new role or refuses with 403 |
| 1009 | message too large: a frame or message above 16 MiB, the Python server's `ws_max_size` |
| 1013 | try again later: the client fell more than 1024 frames behind, or the project would not fit in `COCAT_ROOM_MEMORY_BUDGET` |
| 1011 | internal error while attaching the socket, or the role could not be read again right after it attached |
| 4507 | project storage limit reached: an update would take the project file and its history stamps past `COCAT_ROOM_MAX_MB` or its owner's quota |

A handshake that does not offer the websocket subprotocol `cocat.v2` is refused before the
upgrade with HTTP 426: it comes from a client of the first layout (see
[Project layout v2](#project-layout-v2)). A handshake without a valid cookie or bearer token is refused before the
upgrade with HTTP 401 (sign in again); one from a signed-in user who is not a member of the
project with HTTP 403.

The upgrade answer (HTTP 101) carries a `cocat-role` header: `owner`, `editor` or `viewer`, the
role the socket was admitted with. The server drops the updates of a viewer, so a client
uses the header to refuse a viewer's edits before they reach its document, and to drop
local changes a viewer's cache holds that the server never took (see the Python package's
read-only projects). Clients that do not read it work as before. A role change closes the
member's sockets with 4403, so the next handshake reports the new role.

## Catalogue API

Read-only JSON and VOTable over plain HTTP, for tools that do not speak the sync protocol
(SciQLop, a notebook, curl). Every route needs the `fastapiusersauth` cookie, answers 401 to
an anonymous request and 404 to a non-member, and never keeps a project open: a closed project is
read from its file for the length of the request and dropped again.

```sh
# the room's catalogues, with their event counts
curl --cookie "fastapiusersauth=$TOKEN" \
  http://localhost:8000/api/rooms/my-project/catalogues

# one catalogue
curl --cookie "fastapiusersauth=$TOKEN" \
  http://localhost:8000/api/rooms/my-project/catalogues/6f9c1a52-0b0d-4f4e-9d5e-1b2c3d4e5f60

# the same catalogue as a VOTable
curl --cookie "fastapiusersauth=$TOKEN" \
  -o catalogue.xml \
  http://localhost:8000/api/rooms/my-project/catalogues/6f9c1a52-0b0d-4f4e-9d5e-1b2c3d4e5f60.votable

# one page of events inside a time window
curl --cookie "fastapiusersauth=$TOKEN" \
  'http://localhost:8000/api/rooms/my-project/catalogues/6f9c1a52-0b0d-4f4e-9d5e-1b2c3d4e5f60/events?start=2025-01-01T00:00:00Z&stop=2025-02-01T00:00:00Z&limit=100'
```

Times come back as `YYYY-MM-DDTHH:MM:SS.ffffffZ`; a `start` or `stop` argument accepts that
form, Python's `2025-01-31 10:00:00`, or an explicit offset (`+02:00`), and an argument
without an offset means UTC. Events are ordered by `(start, uuid)`.

**Overlap.** Either bound may be omitted, and an event is returned when its interval overlaps
the requested one (`event.stop >= start` and `event.start <= stop`), so an event that begins
before the window and runs into it is included. A `start` after the `stop` is 422.

**Paging.** `limit` defaults to 1000 and must be between 1 and 10 000. The response carries
`"next"`: an opaque cursor for the following page, or `null` when nothing follows. Send it
back unchanged as `cursor`. The cursor names the `(start, uuid)` of the last event returned,
so events sharing a start time are neither repeated nor skipped.

A malformed time, cursor or limit is 422, an unknown catalogue is 404 `{"detail": "Catalogue
<uuid> not found"}` and a room too big for `COCAT_ROOM_MEMORY_BUDGET` is 503 `{"detail":
"Server memory budget reached"}`. The `.votable` route answers
`application/x-votable+xml` with `content-disposition: attachment; filename="<uuid>.xml"`,
and a catalogue whose events disagree on an attribute's presence or type is 422 with the
exporter's message. The file reads back through cocat's `import_votable`.

## Groups

A group has one owner and any number of members. A project is shared with a group by a grant
(`editor` or `viewer`), and the role a user holds on a project is the highest of their direct
membership and every grant to a group they belong to. A grant can never be `owner`:
ownership only exists as a direct membership. Losing a group, a membership or a grant closes
the sockets that carried it (4403) immediately.

| Route | Who | What |
|---|---|---|
| `GET /api/groups`, `POST /api/groups` | any user | your groups; create one (you own it) |
| `PATCH`/`DELETE /api/groups/{id}` | group owner | rename; delete (revokes every project it carried) |
| `GET /api/groups/{id}/members` | any member | members, by email |
| `DELETE /api/groups/{id}/members/{user_id}` | owner, or yourself | remove a member (the owner cannot leave) |
| `GET /api/rooms/{room_id}/groups` | any project member | the groups the project is shared with |
| `PUT`/`DELETE /api/rooms/{room_id}/groups/{id}` | whoever may share the project | grant `{"role":"editor"\|"viewer"}` (a group you belong to); unshare |

Group invitations (`POST /api/groups/{id}/invitations`, `GET` to list, `DELETE` to revoke,
plus `POST /api/group-invitations/preview` and `/accept`) always admit a member. When an
email is given and a mailer is configured, the link is mailed and the reply carries no token.

**Who may share a project.** Its owner, and its editors while the project's
`editors_can_share` is true (the default; `GET /api/rooms` lists it for each project). Sharing
is inviting, listing and revoking invitations, changing roles, removing members and granting
groups. Only the owner turns it off or on, with `PUT /api/rooms/{room_id}/editors-can-share`
`{"allowed": false}`, and only the owner deletes the project or transfers it. Editors rename.
Nobody can demote or remove the owner, or grant the owner role.

Removing someone from a project directly does **not** remove group access: while a group they
belong to still holds a grant, they keep the project at the group's role. Remove them from the
group, or unshare the project, to cut access.

## Public catalogues

Public catalogues are projects every signed-in account reads and only curators edit. Each
holds one catalogue, so a client loads only the one it shows. Their ids start with `public.`
(`public.amda.sharedcatalog_22`); nobody owns them. Curators are the administrators plus the
accounts an administrator marks (admin pages, or `set-curator`).

| Route | Who | What |
|---|---|---|
| `GET /api/public` | any user | the index: `id`, `name`, `folder` (`"AMDA/Solar wind"`), `source`, `description`, your `role`, `changed`, `count` (events outside the trash) |
| `POST /api/public` | curators | `{"name", "folder"?, "source"?, "description"?, "key"?}` → 201; the id is `public.<key>` (409 if taken), random without a key |
| `PATCH /api/public/{id}` | curators | `{"name"?, "folder"?, "source"?, "description"?}` → 200 with the entry; a field left out stays, a blank one is cleared (not the name); 404 for an id that is not public |

A public catalogue is then a project like any other: the socket, history, catalogues and
export routes answer a reader as a viewer and a curator as an editor, and a reader can copy
a version into a project of their own. Curators rename and delete one. Members, invitations,
groups, sharing, leaving and transfer answer 409 for everyone. They are not in
`GET /api/rooms`. The server refuses to start, and the import to run, when a project that
is not public has a `public.` id (only an id imported from Python can).

Every account reads a public catalogue, so nobody's email shows there. The socket relays no
presence in either direction. A reader's history names each author "a curator" or
"a contributor", with no account id; curators still see emails and ids. A submission's author sees its reviewer as
"a curator".

### Submissions

Everyone else submits. A submission holds the catalogue and its events as they were when it
was submitted (a Yjs update); later edits do not change it. Only its author and the curators
see it: anyone else gets 404.

Snapshots stay on the server, so they count against the author's storage quota, like their
projects, whatever became of them. One snapshot holds at most 50 MB, and an author has at most
20 submissions waiting for a curator (409 beyond).

| Route | Who | What |
|---|---|---|
| `POST /api/public/submissions` | editors of the project | `{"room", "catalogue", "title"?, "folder"?, "description"?, "note"?}` → 201, a new catalogue (the title defaults to its name); 413 when it is over 50 MB or over what your storage has left |
| `POST /api/public/{id}/proposals` | any user | `{"room"?, "name"?}` → 201 `{room, name, catalogue}`: a copy of the public catalogue (same uuids) in one of your projects or a new one; the server remembers where it came from |
| `POST /api/public/{id}/copies` | any user | `{"room"?}` → 201 `{room, name, catalogue}`: a copy of the public catalogue (same uuids and name) in one of your projects or a new one named after it, to use; it does not link back |
| `GET /api/rooms/{id}/proposals` | members | the project's catalogues copied that way: `catalogue`, `target`, `name` |
| `POST /api/public/submissions/updates` | editors of the project | `{"room", "catalogue", "note"?}` → 201, the copy as the public catalogue's next version |
| `GET /api/public/submissions` | any user | your own; a curator's lists everyone's unless `?mine=true`. Pending first |
| `GET /api/public/submissions/{n}` | author, curators | the submission, `changes` (what accepting does, against the public catalogue now) and `stale` (it changed since the copy) |
| `POST /api/public/submissions/{n}/review` | curators | `{"accept", "note"?}`. Accepting a new one makes `public.<slug of the title>`; an update makes the public catalogue equal to the submission, events it left out go to the trash. Either is one change by the author, saved as the version "Submission n: title" |
| `POST /api/public/submissions/{n}/withdraw` | author | a pending one only (409 otherwise) |

`GET /api/me` says `is_curator`.

## Web UI

Server-rendered pages under `{COCAT_PROXY_PREFIX}/ui/`, with the paths and messages of the
Python UI. Every action is a form POST answered by a 303 redirect. Three small static
scripts carry the browser side: `fragment.js` moves a mailed `#token` into a form field,
`confirm.js` asks before a destructive submit, and `copy.js` copies a share link.

| Page | Path |
|---|---|
| Sign in, resend verification | `/ui/login` |
| Sign in with a provider | `/ui/oauth/{name}/login` |
| Sign up | `/ui/signup` |
| Forgot / reset password | `/ui/forgot-password`, `/ui/reset-password#token` |
| Verify an address | `/ui/verify#token` |
| Account, confirm email | `/ui/account`, `/ui/confirm-email` |
| Projects, one project | `/ui/rooms`, `/ui/rooms/{id}` |
| Accept a project invitation | `/ui/invitation#token` |
| Groups, one group | `/ui/groups`, `/ui/groups/{id}` |
| Accept a group invitation | `/ui/groups/join#token` |

- Sessions are the same `fastapiusersauth` cookie as the API. Its value is an opaque
  random token; the server stores only its hash, so logging out, changing or resetting the
  password, and deactivating an account end the sessions that must stop.
- Any request that is not a `GET`, `HEAD` or `OPTIONS` coming from another site is refused
  with 403, on every route: the `Origin` (when sent) must match `Host`, and the
  `Sec-Fetch-Site`, when the browser sends it, must be same-origin or same-site. A client
  that sends neither header, such as the Python client or SciQLop, still passes.
- `?next=` after login only ever leads to a path on this site.
- Login, sign-up, forgot-password and resend are rate-limited (10 per minute per client).
- A message that names something (a project, a group, an address) travels in a short-lived
  `cocat_flash` cookie written by the action's 303 and cleared by the next page view. A
  share link made on a project or group page rides the same way in `cocat_share_link`.
- Invitation tokens stay in the URL fragment or a POST body, so they reach neither an
  access log nor a `Referer` header.

Two deliberate differences from the Python UI:

1. With mail configured, signing up with a taken address looks like a fresh sign-up, and
   the address's owner is mailed about the attempt. Without mail, it still says the
   address is registered.
2. The forgot-password and resend mails are sent in the background, so the answer time
   doesn't reveal whether the address has an account.

### Provider sign-in

Sign-in through OpenID Connect is configured with `COCAT_OAUTH_PROVIDERS`, a JSON list
whose entries follow the Python configuration:

```sh
export COCAT_PUBLIC_URL=https://cocat.example.org
export COCAT_OAUTH_PROVIDERS='[
  {"name": "orcid",
   "discovery_url": "https://orcid.org/.well-known/openid-configuration",
   "client_id": "APP-...",
   "client_secret": "...",
   "scopes": ["openid"]}
]'
```

`discovery_url` is the provider's discovery document; the redirect URI is
`{COCAT_PUBLIC_URL}{COCAT_PROXY_PREFIX}/ui/oauth/{name}/callback`, built from the
configuration and never from the request's `Host` header, so the provider must have that
exact address registered as a client redirect URI. Each configured provider adds a "Sign
in with {name}" link to the login page. The flow uses PKCE and a nonce; the verifier,
nonce and CSRF state ride in one signed `cocat_oidc` cookie (ten minutes, scoped to
`/ui/oauth/`), and no provider token is stored after the callback.

An address the provider returns is believed only when the provider is marked
`"verifies_email": true` and the address parses; otherwise the account gets
`{subject}@{name}.placeholder.cocat.org` and is sent to `/ui/confirm-email` to name a real
address. With a mailer configured, a trusted address links to an existing local account
only when that account is already verified; an unverified one answers
`/ui/login?error=exists`, and without a mailer no association is made at all. A
placeholder address never links to an account and is never verified. A sign-in with a
known subject reaches the same account every time.

Differences from Python beyond the stored-token rule: the Python `Sec-Fetch-Site: none`
refusal is dropped, so a bookmark or a typed `/ui/oauth/{name}/login` URL works (a
cross-site one is still refused with 403); the login route's rate limit answers a 303 with
`?error=too_many` rather than Python's 429; and the JSON API pair
`/auth/{name}/authorize|callback` is not implemented.

Providers are documented as ORCID first: `scopes` is `["openid"]`, which returns no address,
and even with the email scope ORCID does not prove control of it, so a real deployment
leaves `verifies_email` off. Every ORCID user therefore lands on `/ui/confirm-email` and
names a real address after one click from the inbox.

## Durability

Every update is written to the project file before it is broadcast, so a crash of the process
loses nothing a client has seen. The file is fsynced every second, and on unload, delete,
flush and shutdown, so a power loss can lose at most one second of acknowledged edits. The
file is the `0.0.1` append-only format the Python server writes; a record cut by a crash is
detected at open and dropped.

## Moving from the Python server

`import` copies a running Python cocat deployment into this server's database in one
idempotent command:

```sh
cocat-server import --update-dir ./updates --db ./test.db --users ./python.db
```

`--users` is the Python `--db_path` (the SQLite file, not its `.secret` sibling);
`--update-dir` is the directory both servers keep `<room_id>.y` in. `--owner
you@example.org` names the Rust account that takes every room file in `--update-dir` the
imported database does not list: `--users` and `--owner` may be given together, and at
least one is required. `--dry-run` prints the same summary and writes nothing.

Both Python schemas import. The released one stores each user's projects as a JSON list:
every listed project becomes a project named by its id, everyone listed is an editor, and an
owner is picked by the rule of Python's migration (the first member by address whose local
part names the project, else the first member by address); accounts from it come in verified.
The rooms schema (`room`, `membership`, `invitation`, `oauth_account`) is copied as it
stands.

Users keep their id, address, password hash and flags. Login accepts the bcrypt hashes
older Python accounts hold and rehashes them to Argon2 on the first successful sign-in.
Projects keep their name and timestamps, memberships their role, invitations their token hash
(so a live link keeps working on this server) and uses, and linked accounts their provider
and subject; the provider tokens are dropped.

The Python database is opened read-only. Project files are rewritten in the v2 layout (next
section); running the command twice changes nothing the second time. The import refuses to run when `--db` and `--users` name the
same file, and stops before writing anything when an address already belongs to an account
here.

Run the import on a copy of the project files, and keep the Python database and the original
project files until the Rust server has run for a while: they are the way back.

### Project layout v2

Python cocat stores projects in the first layout. This server and `cocat_rs` only use layout
v2, and the two do not mix:

- `import` migrates every project file in `--update-dir` to v2 by appending one record.
- The server refuses to serve a project file still in the first layout (an error log names the
  project): run the import on it.
- Python cocat and SciQLop plugin versions built on it do not offer the `cocat.v2`
  subprotocol, so their handshake is refused with 426 before they can write the first
  layout into a v2 project. Users update the plugin.
- A `cocat_rs` cache holding the first layout is discarded when the project opens, and the
  project is fetched again from the server.

Session cookies do not survive the move. The Python server signs them and this one keeps
its own session table, so every user signs in once again after the switch.

## API documentation

`/docs` serves the Swagger UI page and `/openapi.json` the OpenAPI 3.1 document behind it;
both are public, so the shape of the API can be read before signing in. The document covers
every JSON route (`/api`, `/auth`, `/users` and the two legacy room routes), names the
`fastapiusersauth` cookie as its `apiKey` security scheme, and marks the operations that need
a session. The rendered pages and the websocket handshake are not part of it.

Behind a reverse proxy, `COCAT_PROXY_PREFIX` reaches the document's `servers` entry and the
URL the page fetches, while the routes themselves stay where the proxy forwards them.

## Metrics

`COCAT_METRICS_BIND=127.0.0.1:9187` serves the Prometheus text at
`http://127.0.0.1:9187/metrics`, on its own listener so nothing appears on the public port.
Labels are fixed route templates, never a project id, an address or a request URI, and the
counters on the websocket path allocate nothing per frame. Databases counts and file sizes
are sampled every 60 seconds, so a scrape never queries the database or walks the disk.

With `COCAT_METRICS_TOKEN` set, the endpoint answers 401 with `WWW-Authenticate: Bearer`
unless the request carries `Authorization: Bearer <token>`, compared in constant time.

```yaml
scrape_configs:
  - job_name: cocat
    authorization:
      credentials: <the token>
    static_configs:
      - targets: ["127.0.0.1:9187"]
```

## Python package

`cocat_rs` is the Python package: catalogues and events, kept on your computer and shared
with others through a cocat server.

```sh
pip install cocat-rs
# or, from a checkout of this repository
maturin develop -m crates/cocat-py/Cargo.toml
```

### Getting started

```python
import cocat_rs as cc

cc.login()                          # prints a link: open it, sign in, press Allow
project = cc.open("my-project")     # by name or id; cc.open() is your own project

shocks = project.create_catalogue(name="shocks", author="you@example.org")
event = project.create_event(start="2020-01-01T00:00:00Z", stop="2020-01-01T00:10:00Z",
                             author="you@example.org", tags=["shock"])
shocks.add_events([event])

project.commit(name="first draft")  # a saved version of the project
project.history()                   # every version, newest first
```

- `cc.login()` prints a link and a code, and opens the browser when there is one. Sign in on
  that page and press Allow. The page can be on another device: this works on JupyterHub and
  over SSH too.
- The sign-in is kept in your system's keyring, so the next Python session is signed in
  already. Without a keyring it lasts until Python stops, and a warning says so.
- No browser at all: `cc.login("you@example.org", "password")`. Nothing is stored; it lasts
  until Python stops.
- `cc.logout()` signs out on the server and forgets the sign-in.
- The server is the public one, `cc.DEFAULT_SERVER`. For another one, use
  `cc.login("https://...")` or set the `COCAT_SERVER` environment variable.
- When you are not signed in, or the sign-in was revoked from the account page, a call raises
  `cc.NotSignedIn`: "Not signed in to <server>: run cocat_rs.login()".
- `cc.open` waits until the project is in step with the server (30 s at most). The project is
  kept in your user cache folder, so it also works offline; your changes are sent when the
  server is back. Give `cache="some/folder"` to keep it elsewhere.

More of what you can do:

```python
cc.account()                                   # who you are: id, email, is_curator
cc.projects()                                  # your projects: id, name, role
cc.public_catalogues()                         # the shared reference catalogues
cc.copy_public_catalogue("public.shocks")      # a copy in a new project of yours, to use

info = cc.create_project("Bepi MSA")
cc.invite(info.id, "editor", email="colleague@example.org")
cc.members(info.id)                            # who is in it, with their roles

first = project.history()[-1]["id"]
project.restore(first)                         # the project as it was then
project.copy_version(first, "trial")           # that version as a new project
project.submit(shocks, note="from my thesis")  # offer a catalogue to the public ones
```

Two servers at once: `cc.Client("https://...")` has the same functions as methods
(`client.login()`, `client.open(...)`, `client.projects()`...).

An application with its own window (SciQLop) signs in in two steps, so it can show the link
first and wait in a thread:

```python
sign_in = cc.Client(server).start_sign_in(app="SciQLop")  # named "SciQLop on <machine>"
show(sign_in.url, sign_in.code)
sign_in.wait()          # blocks; NotSignedIn when denied, expired or sign_in.cancel()-ed
```

### Advanced: urls and cookies

Applications that keep the session themselves (SciQLop) use `cocat_rs.raw`: the same
functions with the server's url and a session spelled out, `raw.projects(url, cookie)`,
`raw.history(url, cookie, project_id)`...: each `cc.` function below has a `raw.` twin.
`cookie = raw.login(url, email, password)` gives a session; so does `cc.Client(url).session`
once signed in. `raw.sign_out(url, cookie)` ends one.

Before 0.3.0 the package said room: `Room`, `RoomInfo`, `rooms()` and `room_id=`. Since
0.4.0 only `Project`, `ProjectInfo`, `projects()` and `project_id=` exist.

**Script mode** works on the cached copy, as cocat's own API does:

```python
from cocat_rs import api

api.set_config(host="http://localhost:8000", file_path="/tmp/cocat", project_id="my-project")
api.log_in("you@example.org", "secret")
catalogue = api.create_catalogue(name="events", author="you@example.org")
api.save()
```

Each account keeps its own cache in a folder named after its account id under `file_path`
(`/tmp/cocat/<account id>/`), so two accounts sharing `file_path` never touch each other's
offline edits. Caches written directly in `file_path` by earlier versions are left as they
were, not read, and named in one warning: open one with `Project.open` to send what it holds.

**Client mode** keeps the same `Project` in step with a server through the websocket.
`connect` takes the server's address, the one you log in to (with its path prefix behind a
proxy), and adds `/room/<id>` itself:

```python
import cocat_rs

url = "http://localhost:8000"
project = cocat_rs.Project.open("/tmp/cocat", "my-project")
project.connect(url, credentials=("you@example.org", "secret"))
project.wait_synced(10)

print(project.status)                   # offline, connecting, synced, unauthorized, no_access
print(project.role, project.read_only)  # "editor", False

event = project.event("<a uuid>")
print(event.catalogues)                 # the uuids of the catalogues listing it
print(project.event_arrays([event], attributes=True))
```

`connect` signs in with the credentials and keeps them, so the project signs in again by
itself when the session expires. A wrong password raises
`Unauthorized` right away. If you already have a session cookie (SciQLop gets one from the
browser sign-in), pass it instead: `project.connect(url, cookie)`. Without credentials, a
session the server drops leaves the project `unauthorized` until `set_cookie` gives it a
new one. The `cc.` functions below use the sign-in from `cc.login()`; with a cookie of
your own, use their `cc.raw.` twins.

**Read-only projects.** `project.role` is the role the server named at the last connect
(`owner`, `editor`, `viewer`), remembered next to the cache, so a project opened offline keeps
it; it is `None` until a first connect. A viewer's project is `read_only`, as is a past
version from `Project.at`: every write raises `RuntimeError` ("this project is read-only for
you (viewer)") before it touches the project, since the server would drop it. When a viewer
connects with a cache holding changes the server never took (made before the role was known,
or by cocat_rs 0.3.0), the cache is dropped and the project reloaded from the server, with one
warning in the `cocat_client` log. Give each account its own cache folder (script mode does):
the drop clears whatever that folder holds for the project.

When an owner changes your role while you are connected, the project reconnects at once and
picks up the new role: `role` and `read_only` follow, and a project that became read-only drops
the changes the server never took, as above. Two statuses mean the project stopped for good:
`unauthorized` means your session is gone, so sign in again; `no_access` means you are no
longer in this project (you were removed, or it was deleted), and signing in again will not
help.

Events carry more than a range: `status` (`preliminary`, `confirmed`, `reviewed`, `final`,
`rejected`), `confidence` (0 to 1), `start_err` and `stop_err` (seconds), `origin` (what made
the event, from `project.create_origin(...)`), `source_id`, `created`, `comment` and a
`shape`. All are settable and all are keyword arguments of `create_event`. Attributes register
their type in the project (`project.attribute_types()`); writing another type warns but is kept.

```python
event = project.create_event(start, stop, author="you@example.org", confidence=0.8,
                             tags=["shock"], attributes={"density": 12.5})
event.status = "reviewed"
event.delete()                          # to the trash: hidden from every list
project.trash()                         # [event]
event.restore()                         # or event.purge(), for good
```

A **smart catalogue** has a `rule`: its members are its listed events plus what the rule
yields, computed on every read, so it follows the project as people edit it.

```python
smart = project.create_catalogue(name="strong shocks")
smart.rule = {"filter": {"all": True},
              "where": {"and": [{"contains": ["tags", "shock"]},
                                {"cmp": ["confidence", ">=", 0.7]}]}}
smart.events                            # the events it holds now
smart.event_arrays()                    # every member; a merged interval has uuid None
                                        # and the events it came from in "sources"
snapshot = smart.materialize("strong shocks, today")   # a static copy
```

A malformed rule raises `ValueError` saying where: `rule.where.cmp: unknown operator '=~'`.
The rule format (sets, interval operators, predicates) is in
`docs/superpowers/specs/2026-11-07-catalogue-engine-v2-design.md`, section 3.

**tscat** databases and JSON files import into a project, keeping uuids, so importing again
updates rather than duplicates. A rating becomes a confidence (rating / 10), removed items go
to the trash and dynamic catalogues become smart ones.

```python
from cocat_rs import tscat

tscat.import_database(project)          # the local tscat database: pip install 'cocat-rs[tscat]'
tscat.import_json(project, "export.json") # tscat's JSON, no tscat needed
tscat.export_json(project, [smart], "out.json")
```

tscat writes a dynamic catalogue's predicate into its JSON as Python text, so `import_json`
keeps such a catalogue's members without a rule, and warns.

A **shape** is a list of `(datetime, value)` corners in the time plane: the value is on the
catalogue's shape axis, which a catalogue may name as `(quantity, unit)`, such as
`("frequency", "kHz")` or `("energy", "eV")`. The JSON API names a corner's second
coordinate `value` (cocat-rs 0.5.2 and before named it `frequency`).

```python
catalogue.shape_axis = ("energy", "eV")  # None when not said
```

**TFCat** files (radio catalogues of time–frequency shapes, doi:10.25935/6068-8528) import
as one catalogue each, next to VOTable. A polygon becomes the event's `shape`; any other
geometry is kept as it is, in the event attribute `tfcat_geometry`, and gives the event its
time span. The feature's `id` becomes `source_id`. The file's coordinate system and field
descriptions stay on the catalogue (`tfcat_crs`, `tfcat_fields`), so an export writes them
back. The file's spectral axis becomes the catalogue's `shape_axis`, and an export writes the
shape axis back into the coordinate system; a catalogue without one exports as frequency in
Hz. A file without a coordinate system (`crs`) is refused.

```python
bursts = project.import_tfcat("bursts.json")            # named by the file's title
cocat_rs.export_tfcat(bursts, "out.json")
cocat_rs.export_tfcat(plain, "out.json", value_range=(1e3, 2e4))
text = cocat_rs.tfcat_json(bursts)                      # the same, as text
```

An event without a shape is written as a rectangle over `value_range`, else over the band
of the catalogue's shaped events; with neither, the export raises `ValueError`.

Shapes and the shape axis travel in all three formats and come back as they were. VOTable
writes the shape axis as a table `PARAM` named `shape_axis` (the quantity as its value, the
unit as its `unit`), and each shape as JSON text `[[time, value], ...]` in a `shape` column
(times in RFC 3339 UTC; `[]` for an event without one). tscat JSON writes the same JSON as
an event's `shape` and `{"quantity", "unit"}` as a catalogue's `shape_axis`; tscat keeps both
as attributes. TFCat only knows frequency, wavelength, energy and wavenumber: another axis is
still written, with a warning that the file is not strict TFCat.

**History.** The server keeps every version of a project: a run of changes by one person with
no gap over five minutes, and no longer than an hour. Each version says who made it, when, and how many events it added,
changed or removed. `project.history()` lists them, newest first, as dicts; `catalogue=` keeps
the versions that touched one catalogue or its events. `Project.at` fetches the project as it
was at a version: it is read-only (a write raises `RuntimeError`) and needs no connection once
fetched. `project.restore(version)` makes the project, or one catalogue, equal to a version again, as one
new change by you, so a restore can itself be undone; events created since go to the trash.
A restore is always a version of its own, with `restored_from` set to the version it brought
back. A version's `id` is its first change, so it stays the same, and keeps its name, while
its author goes on editing.
`project.commit()` saves the newest version on purpose: it ends at the project's last change
the server has, with an optional `name` and `message` (a note, up to 2000 characters), and
returns its id. It sends your pending edits first. The next
change starts a new version, and the list marks the saved one with `committed` and its
`message`. Saving again with nothing new raises `cocat_rs.Conflict`.
`project.copy_version(version, name)` makes a new project, owned by
you, that starts as the version: same catalogues, events (with their uuids), rules and trash.
It returns the new project's id. Any member may copy, a viewer too; the copy counts against
your storage quota. The original and its history stay as they were.

```python
versions = project.history()
first = versions[-1]["id"]
project.name_version(first, "before the import")
project.commit(name="paper v1", message="as submitted")
project.restore(first)                         # False when nothing differed
trial = project.copy_version(first, "my-project (copy)")
```

**Public catalogues.** `cc.public_catalogues()` lists them as dicts (`id`, `name`,
`folder`, `source`, `description`, your `role`, `changed`, `count`). Open one like any project, with its
id: a curator gets an editable project, everyone else a read-only one. A curator makes one
with `cc.create_public_catalogue(name, folder=None, source=None, description=None,
key=None)`, which answers its id (`public.<key>`, or a random one); anyone else gets
`Forbidden`, a key already taken `Conflict`. `cc.update_public_catalogue(id,
name=None, folder=None, source=None, description=None)` changes its entry: `None` keeps a
field, `""` clears it. `cc.copy_public_catalogue(public_id, project_id=None)`
copies one, with its name and events, into one of your projects (a new one named after it
without `project_id`) to use there, and answers `{"project_id", "name", "catalogue"}`.

Submitting: `cc.submit_catalogue(project_id, catalogue_uuid, title=None,
folder=None, description=None, note=None)` offers one of your catalogues and answers the
submission's id. `cc.propose_change(public_id, project_id=None, name=None)` copies a
public catalogue into one of your projects (a new one without `project_id`) and answers
`{"project_id", "name", "catalogue"}`; change it there, then `cc.submit_update(
project_id, catalogue_uuid, note=None)`. `cc.submissions(mine=False)` lists yours
(a curator's: everyone's, unless `mine`), with `status` and the curator's `review_note`.
Curators call `cc.review_submission(id, accept, note=None)`; authors
`cc.withdraw_submission(id)`.

```python
index = cc.public_catalogues()
shocks = cc.open(index[0]["id"])                 # read-only unless you are a curator
```

**Managing a project.** The calls behind the web app's Share dialog, for a tool that shows who
has access or shares a project. Members, invitations and groups come back as dicts shaped like
the server's JSON; ids of members are account ids (uuid strings), ids of invitations integers.
Whoever may share the project may change roles, remove others, invite and see or revoke the
invitations: the owner always, and editors unless the owner turned that off with
`cc.set_editors_can_share(project_id, False)` (owner only; `ProjectInfo.editors_can_share`
says which). Everyone else gets `Forbidden`. Nobody can change the owner's role or remove the
owner. Every member may list the members and the groups. `role` is `"viewer"` or `"editor"`: the server refuses
anything else with a `ServerError` (422). An invitation with an `email` admits only that
address, once; without one it is a link anyone may use, `max_uses` times (no limit by
default). It expires after `expires_in_days` (1 to 90, 14 by default). `link` is the page
that accepts it, on the address you called. `project_url` makes no call: it is the project's
page in the web app, with its Share dialog open when `share=True`.

```python
info = cc.create_project("Bepi MSA")       # a ProjectInfo, you own it
cc.members(info.id)        # [{"user_id": ..., "email": ..., "role": "owner"}]
cc.set_role(info.id, user_id, "viewer")
cc.remove_member(info.id, user_id)
created = cc.invite(info.id, "editor", email="colleague@example.org")
created["link"]                               # <url>/ui/invitation#<token>, to send them
cc.invitations(info.id)    # id, email, role, expires_at, uses, max_uses
cc.revoke_invitation(info.id, created["id"])
cc.project_groups(info.id) # [{"id": ..., "name": ..., "role": ...}]
cc.project_url(info.id, share=True) # <url>/ui/rooms/<id>/browse#share
```

**Errors.** When the server refuses a call (`login`, `projects`, `history`, `Project.at`,
`restore`, `name_version`, `commit`, `copy_version`, and the calls that manage a project), the
error says why. `cocat_rs.Unauthorized` (401)
means the session expired or there is none: log in again. `cocat_rs.Forbidden` (403) means
your role does not allow it, `cocat_rs.ServerNotFound` (404) that the project or version does
not exist, and `cocat_rs.Conflict` (409) that the call clashes with the project, e.g. nothing
to save. Any other refusal is their base, `cocat_rs.ServerError`. Each carries `status` and
`detail`, the server's own text, which is also the message. `ServerError` subclasses
`ValueError`, so older `except ValueError` code still catches it, and `ServerNotFound` is
also a `cocat_rs.NotFound`.

```python
try:
    project.commit()
except cocat_rs.NotSignedIn:
    cocat_rs.login()
except cocat_rs.Conflict as error:
    print(error.detail)   # Nothing changed since the last saved version.
```

`credentials` is what lets the client recover a session the server ended (a password change,
a deactivation, a restart with a new database): the sync task logs in again once, swaps the
cookie and retries, and only a refusal right after a fresh login, or a login that fails, ends
in `unauthorized`. Without `credentials` a refused session is final, as before.
`project.set_cookie(...)` replaces the cookie of a running connection.

A listener hears the project's changes on the loop it was registered from:

```python
class Listener:
    def status(self, value):
        print("the project is now", value)

    def catalogue_added(self, uuid):
        print("new catalogue", uuid)

    def changes_done(self):
        pass

handle = project.listen(Listener())
handle.close()
```

`status` is called once with the current status right after registration, then on every
change. `role` works the same way with your role in the project (`"viewer"`, `"editor"`,
`"owner"`, or `None` before the first connection): when an owner changes it, the project
reconnects by itself and `role` is called with the new one. A method the listener does not have is skipped, and one that raises is logged without
stopping the batch. Unknown catalogues and events raise `cocat_rs.NotFound`, which subclasses
both `LookupError` (what Python's mappings raise) and `RuntimeError` (what this package raised
before it named the case), so both `except LookupError` and old `except RuntimeError` code
still catches it.

## Tests

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

`bench/` holds the phase 0 gate harness (see `bench/README.md`).
