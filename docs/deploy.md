# Running cocat-server

This guide takes you from nothing to a cocat server behind a reverse proxy, with mail, the
catalogue web app, metrics and backups. The [README](../README.md) is the reference for every
option; this page says which ones an operator needs and how they fit together.

## The image

Each release publishes `ghcr.io/sciqlop/cocat-server`, tagged with the version (`0.7.0`) and
`latest`. The release workflow (`.github/workflows/publish.yml`) builds it from
`docker/Dockerfile` when a `v*` tag is pushed.

To build it yourself from a checkout:

```sh
docker/build.sh my-registry.example.org/cocat-server:dev          # build only
docker/build.sh my-registry.example.org/cocat-server:dev --push   # build and push
```

The script uses podman when it is installed, docker otherwise. Inside a toolbox or
distrobox container it calls the host's podman with `--remote`. A name without a tag means
`:latest`. The build compiles the server and the web app and takes a while. Let it print to
the terminal or to a file: piping it into a filter that exits early (`| head`) stops the
build silently.

## Running it

```sh
podman run -d --name cocat -p 6570:6570 \
    -v cocat-storage:/storage -v cocat-db:/db -v cocat-log:/log \
    -e COCAT_PUBLIC_URL=https://cocat.example.org \
    -e COCAT_PROXY_PREFIX=/cocat \
    --env-file mail.env \
    --stop-timeout 30 \
    ghcr.io/sciqlop/cocat-server:latest
```

`docker run` takes the same arguments.

| Mount | Holds |
|---|---|
| `/storage` | the project files: `<room_id>.y` and its history stamps `<room_id>.y.h`; a workspace adds one `<project>%2F<document>.y` (and `.y.h`) per document, and its whole files under `blobs/` |
| `/db` | `cocat-rs.db` (accounts, projects, memberships, sessions) and `cocat-rs.db.secret` |
| `/log` | kept for the Python image's contract; the server prints to the container output (`podman logs cocat`) |

The server listens on port 6570 inside the container (`PORT`). It runs as the `cocat` user
(uid 1000), so a host directory used as a mount must be writable by that user (rootless
podman: `podman unshare chown 1000:1000 <dir>`). With SELinux, add `:Z` to a bind mount.
`podman stop` sends `SIGTERM`: the server flushes every open project before it exits, so give
it time (`--stop-timeout 30`).

## Configuration

Everything is environment variables. The README's
[Environment](../README.md#environment) table gives the exact rules: defaults, accepted
values, and which mistakes stop the server at startup. The ones an operator sets:

| Group | Variables | What for |
|---|---|---|
| Public address | `COCAT_PUBLIC_URL`, `COCAT_PROXY_PREFIX` | the scheme and host users reach; the path the proxy mounts the server under. Mailed links, provider redirects and the sign-in links apps show are built from both. |
| Proxy | `COCAT_TRUSTED_PROXIES` | how many proxies stand in front, so rate limits see the real client. The image sets 1. |
| Secret | `COCAT_SECRET` | signs mailed links and the provider sign-in cookie. Unset, the server keeps one in `/db/cocat-rs.db.secret`: back it up with the database. |
| Mail | `COCAT_SMTP_HOST`, `COCAT_SMTP_PORT`, `COCAT_SMTP_STARTTLS`, `COCAT_SMTP_USERNAME`, `COCAT_SMTP_PASSWORD`, `COCAT_MAIL_FROM`, `COCAT_MAIL_DIR` | see [Mail](#mail) |
| Web app | `COCAT_WEB_DIR` | the catalogue web app's bundle; the image sets it to `/usr/share/cocat/web` |
| Quotas | `COCAT_ROOM_MEMORY_BUDGET`, `COCAT_ROOM_MAX_MB`, `COCAT_OWNER_MAX_MB`, `COCAT_FILE_MAX_MB` | memory ceiling for open projects; largest project file; total per owner; largest whole file in a workspace (20 by default, counted in the owner's total) |
| Metrics | `COCAT_METRICS_BIND`, `COCAT_METRICS_TOKEN` | see [Metrics](#metrics) |
| Sign-in providers | `COCAT_OAUTH_PROVIDERS` | OpenID Connect providers such as ORCID; see the README's [Provider sign-in](../README.md#provider-sign-in) |

Keep secrets (the SMTP password, the metrics token, provider client secrets) in a file only
you can read, passed with `--env-file`, rather than on the command line.

## Behind a reverse proxy

The server speaks plain HTTP; let the proxy terminate TLS. The proxy must:

1. strip the prefix before forwarding (the server serves `/ui/...`, not `/cocat/ui/...`),
   and the server must be told the prefix with `COCAT_PROXY_PREFIX`;
2. pass the websocket upgrade, since projects sync over `/room/<id>`;
3. pass the client's `Host` header unchanged: the server refuses a form POST whose `Origin`
   does not match `Host`, so a proxy that rewrites `Host` breaks every form;
4. append the client address to `X-Forwarded-For`, so rate limits apply per client.

An nginx example for `https://cocat.example.org/cocat/`:

```nginx
location /cocat/ {
    proxy_pass http://127.0.0.1:6570/;          # the trailing slash strips /cocat
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_set_header Host $http_host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_read_timeout 1h;                       # keep idle project sockets open
}
```

To serve the server at the root of its own host, use `location /`,
`proxy_pass http://127.0.0.1:6570;` and no `COCAT_PROXY_PREFIX`.

Then check that `https://cocat.example.org/cocat/docs` shows the API documentation and
`/cocat/ui/login` the sign-in page.

### Protecting sign-up from bots

Sign-in, sign-up, password reset and resend are rate-limited per client (README,
[Login rate limit](../README.md#login-rate-limit)). On a public server you may also put a
bot filter (a proof-of-work page, a captcha) in front of the account pages and their API:
`/ui/{signup,login,forgot-password,reset-password,request-verify}` and
`/auth/{register,forgot-password,reset-password,request-verify-token}`, under your prefix.
Leave `/auth/jwt/login`, `/api`, `/users`, `/room` and the project pages open: SciQLop and
scripts sign in and sync there and cannot solve a challenge.

## The first administrator

Create an account and make it an administrator with the wrappers the image ships:

```sh
podman exec cocat create-user admin@example.org 'a long password'
podman exec cocat set-superuser admin@example.org
podman exec cocat set-superuser admin@example.org --revoke   # to take it back
```

Curators look after the public catalogues. Every administrator is one; mark someone else
with `podman exec cocat set-curator them@example.org` (`--revoke` to take it back), or from
the admin pages (Users, Manage).

They run the server's CLI on `/db/cocat-rs.db`. An account created this way is already
verified. The README's [Seeding](../README.md#seeding) lists the full commands;
`add-user-to-room` and `remove-user-from-room` have wrappers too.

These commands only change the database. A running server reads every open socket's role
again every 30 seconds, so a revoked role or removed member is cut off within that time.
The admin pages cut them off at once.

## Public catalogues from AMDA

The AMDA provider replaces the `tools/amda_public.py` script. It copies AMDA's shared
catalogues and time tables (222 lists, about 316,000 intervals) into the server as public
catalogues, one per list, and keeps them in step with AMDA. Like every provider it is off
until an administrator turns it on: admin page, "Catalogue providers", AMDA. It runs daily at
04:00 UTC by default; "Refresh now" runs it at once.

Each list becomes `public.amda.<AMDA id>` (`public.amda.sharedcatalog_22`), filed under
`AMDA/` plus its AMDA folder, with AMDA's description. It holds one catalogue named like the
list; a catalogue's columns become event attributes. A refresh adds new intervals, rewrites
changed columns and moves intervals AMDA dropped to the trash; nothing is duplicated.

A server where the script already ran keeps its AMDA catalogues: the provider uses the same
ids and uuids, so its first run changes nothing in a list AMDA did not change. Curators can no
longer edit those catalogues once the provider follows them. A list AMDA cannot give is left as
it was and named on the admin page; the other lists are refreshed.

## The catalogue web app

The web app (`crates/cocat-web`) is a static bundle that trunk builds; the image builds it
and copies it to `/usr/share/cocat/web`, and `COCAT_WEB_DIR` points the server at it. Without
that variable, or with a directory missing its `index.html`, a project's `/browse` page answers
503 "The catalogue browser is not installed on this server". Signing in then lands on the
server's own pages: the projects list (`/ui/rooms`), each project's page (`/ui/rooms/<id>`:
members, groups, invitations and share links) and the groups pages (`/ui/groups`). With the
bundle installed, those pages open the app instead, where the Share dialog and the groups view
do the same work.

The page tells the app where its project's websocket is. With `COCAT_PUBLIC_URL` set that is an
absolute `wss://` (or `ws://`) URL under `COCAT_PROXY_PREFIX`; without, a path the browser
resolves against the page, so it follows whatever scheme and host the visitor used. Either
way the proxy must pass the websocket upgrade (see above).

To build the bundle by hand: `rustup target add wasm32-unknown-unknown`,
`cargo install trunk --version 0.22.1 --locked`, then `trunk build --release` in
`crates/cocat-web` (the bundle lands in `crates/cocat-web/dist`).

The browser holds the whole project. Measured 2026-11-08 on a copy of a large production
project (8.4 MB file, 11k events, 4 catalogues) with a debug server and headless Firefox: first
rows 2.0 s after navigation, the tab's memory up by about 65 MB. A 10k-event generated project:
first rows 1.0 s, scrolling at 60 frames per second, filtering in 30 ms.

## Mail

Mail carries account verification, password resets and invitations. There are three modes:

- **No mail** (neither `COCAT_SMTP_HOST` nor `COCAT_MAIL_DIR`). Sign-up and password reset
  have nothing to deliver; an invitation returns its token in the response, for you to pass
  on.
- **An SMTP relay.** Copy `docker/mail.env.example`, fill it in, keep it private
  (`chmod 600`), and pass it with `--env-file`. The port defaults to 587 with STARTTLS.
- **Files.** `COCAT_MAIL_DIR=/mail`, with an existing directory mounted there, writes each
  message as an `.eml` file. Handy for a test instance.

Both mail modes need `COCAT_MAIL_FROM` and `COCAT_PUBLIC_URL`; the server refuses to start
without them, because every mailed link is built from the public address.

## Metrics

`COCAT_METRICS_BIND` starts a Prometheus endpoint on its own listener, never on the public
port. Keep it where the outside cannot reach it, and set `COCAT_METRICS_TOKEN` (16 characters
or more, e.g. `openssl rand -hex 32`) so a scrape must carry the token:

```sh
podman run ... -p 127.0.0.1:9187:9187 \
    -e COCAT_METRICS_BIND=0.0.0.0:9187 --env-file metrics.env ...
```

Inside the container the listener binds `0.0.0.0` so the published port reaches it; the
`-p 127.0.0.1:...` keeps it on the host's loopback. With host networking, bind
`127.0.0.1:9187` directly.

```yaml
scrape_configs:
  - job_name: cocat
    authorization:
      credentials: <the token>
    static_configs:
      - targets: ["127.0.0.1:9187"]
```

A scraper on another machine needs a route to the endpoint: put a TLS proxy in front of it
that admits only the scraper's addresses and forwards the `Authorization` header. Labels
never carry a user, a project, an address or a URL, so the series are safe on a shared
dashboard. The README's [Metrics](../README.md#metrics) has the details.

## Moving from a Python cocat server

The Rust server reads the Python server's project files and imports its user database
(README, [Moving from the Python server](../README.md#moving-from-the-python-server)).

1. Try it on a copy first. Copy the Python project directory and its user database (the
   database with SQLite's online backup, as in [Backups](#backups), so the Python server
   keeps running), then import into a fresh database:

   ```sh
   podman run --rm --entrypoint cocat-server \
       -v "$PWD/copy/storage:/storage:Z" -v "$PWD/copy/db:/db:Z" \
       -v "$PWD/copy/python:/python:Z" \
       ghcr.io/sciqlop/cocat-server:latest \
       import --update-dir /storage --db /db/cocat-rs.db --users /python/users.db \
       [--owner you@example.org] [--dry-run]
   ```

   Read the summary it prints: users, projects, memberships and invitations. `--owner`
   takes the project files no Python account lists. Run a server on the copy and check that
   accounts sign in and see their projects with the same roles.
2. To switch: stop the Python server, take the final copy, import it without `--dry-run`,
   and start the Rust server on the copied volumes.
3. Keep the Python volumes untouched until the Rust server has run for a while; verify
   logins, projects and daily use first.

The import moves every project to layout v2. From then on Python cocat clients and old
SciQLop plugin versions are refused with HTTP 426: announce the plugin update with the
switch. Session cookies do not carry over, so everyone signs in once more.

Rollback is restarting the Python server on its untouched volumes. Python clients cannot
read the migrated project files, so edits made on the Rust server since the switch are lost,
and so are accounts created there.

## Backups

Copy, together:

- the whole project directory (`/storage`), with its `blobs/` folder: every `<room_id>.y` and `<room_id>.y.h`, and for workspaces every `<project>%2F<document>.y` and `.y.h` plus `blobs/`;
- the database `/db/cocat-rs.db`, with SQLite's online backup, never a plain file copy
  while the server runs:

  ```sh
  sqlite3 /path/to/db/cocat-rs.db ".backup '/backups/cocat-rs.db'"
  ```

- `/db/cocat-rs.db.secret`, unless you set `COCAT_SECRET`. Without it, a restored server
  signs with a new secret and the links already mailed stop working.

Project files are append-only and the server fsyncs them every second. A file copied while
the server writes to it may end in a partial record; the server drops that record when it
opens the project, so a live copy loses at most the last edits. For an exact snapshot, stop
the server, or snapshot the filesystem, first.
