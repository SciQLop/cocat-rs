# Running a development copy

On the `sciqlop` host the Python server runs as the `cocat-server` container (image
`sciqlop/cocat-server:devel`, host network, port 6570, `COCAT_PROXY_PREFIX=cocat`), deployed by
`/usr/bin/update-cocat-server` as the `sciqlop` user, with its data in `/DATA/cocat_server`
(`storage/` for the room files, `db/users.db`). nginx serves it at `/cocat`.

The Rust dev server runs beside it, on its own copy of that data, never on the production
directories:

| | Python production | Rust dev |
|---|---|---|
| container, systemd user unit | `cocat-server` | `cocat-rs-dev` |
| image | `sciqlop/cocat-server:devel` | `sciqlop/cocat-server-rs:devel` |
| port (host network) | 6570 | 6575 |
| URL | `/cocat` | `/cocat-dev` |
| data | `/DATA/cocat_server` | `/DATA/cocat_rs_dev` |
| mail | as configured | written as files to `/DATA/cocat_rs_dev/mail` |

## 0. Once: install the scripts and the nginx route

As root:

```sh
install -m 0755 docker/update-cocat-rs-dev.sh /usr/bin/update-cocat-rs-dev
install -m 0755 docker/seed-cocat-rs-dev.sh /usr/bin/seed-cocat-rs-dev
ss -ltn | grep -q ':6575 ' && echo "6575 is taken, pick another port in both scripts"
mkdir -p /DATA/cocat_rs_dev && chown sciqlop: /DATA/cocat_rs_dev
```

In `/etc/nginx/conf.d/sciqlop.conf`, copy the whole `location /cocat{ ... }` block (it carries the
websocket headers), and in the copy change the three lines:

```nginx
location /cocat-dev {
    rewrite /cocat-dev(.*) $1  break;
    proxy_pass http://localhost:6575;
```

Put it before the `/cocat` block (nginx picks the longest prefix, but keeping them ordered
avoids surprises), then `nginx -t && systemctl reload nginx`.

## 1. Build and push the image

From a checkout, on a machine that can push to the registry:

```sh
podman login registry.lpp.polytechnique.fr:5443   # pushes need a login, pulls do not
docker/build_and_push.sh testing                  # sciqlop/cocat-server-rs:devel
```

## 2. Seed the data, then deploy

As the `sciqlop` user (`su - sciqlop`):

```sh
seed-cocat-rs-dev [owner@example.org]   # copy production, import it
update-cocat-rs-dev                     # pull the image, (re)start cocat-rs-dev
```

`seed-cocat-rs-dev` stops the dev server, deletes `/DATA/cocat_rs_dev`, copies the room files
and the user database while production keeps running (SQLite's online backup for the
database; a room file copied mid-append ends in a partial record the Rust server drops), then
runs `cocat-server import`. The optional address takes the room files no Python account
lists. Read the import summary it prints: users, rooms, memberships and invitations.

`update-cocat-rs-dev` is the same procedure as `update-cocat-server`: pull, keep the previous
container as `cocat-rs-dev_old`, run, regenerate the systemd unit and enable it with `--now`
so systemd restarts it after a crash.

## 3. Check the dev instance

- `https://sciqlop.lpp.polytechnique.fr/cocat-dev/docs` answers and the document loads.
- An imported account signs in at `/cocat-dev/ui/login`; its rooms are listed with the same
  roles as on Python.
- SciQLop pointed at `https://sciqlop.lpp.polytechnique.fr/cocat-dev` opens one room through
  the Python client and shows its catalogues and events; the event count matches production.
- A room's page has an "Open catalogues" button; it opens `/cocat-dev/ui/rooms/<id>/browse`,
  which shows the cocat header and loads the room.
- `podman logs cocat-rs-dev` shows no error; `ls /DATA/cocat_rs_dev/mail` holds the mails a
  sign-up or a reset would have sent.

## The catalogue browser

The browser app (`crates/cocat-web`) is a static bundle that trunk builds; the image builds it
and copies it to `/usr/share/cocat/web`, and `COCAT_WEB_DIR` points the server at it. Without
that variable, or with a directory missing its `index.html`, a room's `/browse` page answers
503 "The catalogue browser is not installed on this server"; everything else works.

The page tells the app where its room's websocket is. With `COCAT_PUBLIC_URL` set that is an
absolute `wss://` (or `ws://`) URL under `COCAT_PROXY_PREFIX`; without, a path the browser
resolves against the page, so it follows whatever scheme and host the visitor used. The nginx
`location` block already carries the websocket headers the app needs.

To build the bundle by hand: `rustup target add wasm32-unknown-unknown`,
`cargo install trunk --version 0.22.1 --locked`, then `trunk build --release` in
`crates/cocat-web` (the bundle lands in `crates/cocat-web/dist`).

The browser holds the whole room. Measured 2026-11-08 on a local copy of the largest cocat-dev
room (8.4 MB file, 11k events, 4 catalogues) with a debug server and headless Firefox: first
rows 2.0 s after navigation, the tab's memory up by about 65 MB. A 10k-event generated room:
first rows 1.0 s, scrolling at 60 frames per second, filtering in 30 ms.

## Bot protection

nginx sends the sign-up and account-recovery routes, pages and API alike
(`/cocat-dev/ui/{signup,login,forgot-password,reset-password,request-verify}` and
`/cocat-dev/auth/{register,forgot-password,reset-password,request-verify-token}`), through the
Anubis instance that already guards `/cdaweb-data/` (`anubis@cdaweb`, a proof-of-work page for
browsers). Unlike the mirror, these routes have no exemption for scripts. `/auth/jwt/login`,
the rooms, the API and the websocket stay open: SciQLop and scripts sign in and sync there. The
block sits in `/etc/nginx/conf.d/sciqlop.conf` before the `/cocat-dev` location; a backup of
the file before the change is in `/root/sciqlop.conf.before-cocat-dev-anubis-*`.

## Metrics

The app exports Prometheus metrics on `127.0.0.1:9188` (`COCAT_METRICS_BIND`), guarded by a
bearer token (`COCAT_METRICS_TOKEN`, in `~sciqlop/.config/cocat-rs-dev/metrics.env`, mode
600). nginx serves them to the cluster on `https://sciqlop.lpp.polytechnique.fr:9187/metrics`
with the site certificate, and both nginx and firewalld admit only `129.104.6.0/24`, the
cluster nodes: their VictoriaMetrics scrapes it every 60 s (job `cocat`, label `env: dev`),
with the token from a Secret on the cluster side. `docker/cocat-metrics-host.sh` (as root)
sets up the token, the nginx listener and the firewall rule; it is idempotent. The series
end up in the public Grafana org: labels carry no user, room, email or address, by design.

## Sending real mail

Without configuration the dev server writes each mail as a file in `/DATA/cocat_rs_dev/mail`.
To send through the lab relay, put the whole mail configuration in
`~sciqlop/.config/cocat-rs-dev/mail.env` (mode 600; `docker/mail.env.example` is the
template: hermes, port 25 with STARTTLS, the account and the sender address), then run
`update-cocat-rs-dev` again. The password never appears in the script or the unit file.

## 4. Refresh the dev data

Run step 2 again. Starting over is quicker than importing on top, and keeps the dev
instance honest. Anything written on the dev server since the last seed is lost.

## Switching production

Not done yet. When it is:

1. Stop the Python container.
2. Take the final copy and import it without `--dry-run`, exactly as in steps 1 and 2.
3. Start `:stable` on the production port with the production volumes.
4. Keep the Python volumes untouched until the Rust server has run for a while; verify logins,
   rooms and the daily use first.

The import moves every room to layout v2 (README, "Room layout v2"). From then on Python
cocat clients and old SciQLop plugin versions are refused with HTTP 426: announce the plugin
update with the switch.

Rollback is restarting the Python container on the untouched Python volumes. The migrated
room files are not readable by Python clients, so edits made on the Rust server since the
switch are lost, and so are accounts created there.
