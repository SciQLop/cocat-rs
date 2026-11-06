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
- `podman logs cocat-rs-dev` shows no error; `ls /DATA/cocat_rs_dev/mail` holds the mails a
  sign-up or a reset would have sent.

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

Rollback is restarting the Python container. Room files are compatible in both directions, but
accounts created on the Rust server between the switch and the rollback are lost.
