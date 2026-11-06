# Running a development copy

The dev instance is a copy of production, never a bind of its volumes: the Python server and
the Rust one keep writing to the same `.y` files and the same SQLite database, so a shared
mount would let one corrupt the other. Everything below happens on the machine that runs the
containers; the production volumes are only read.

`DEV` is the directory that holds the copy, the new database and the logs. The devel image is
`129.104.6.172:32219/sciqlop/cocat-server-rs:devel`.

## 1. Copy the production data

Stop the Python container for the copy: it flushes its open rooms on `SIGTERM`, and a copy
taken while it runs can hold half-written room files or an SQLite journal.

```sh
export DEV=~/cocat-dev
mkdir -p "$DEV/copy/storage" "$DEV/copy/db" "$DEV/db" "$DEV/log"

podman stop cocat
podman cp cocat:/storage/. "$DEV/copy/storage/"
podman cp cocat:/db/users.db "$DEV/copy/db/users.db"
podman start cocat
```

## 2. Import the copy

Dry run first and read the summary: it names how many users, rooms, memberships and
invitations were found. `--users` is the Python database file, `--update-dir` the directory
holding the room files, `--db` the new Rust database. `--owner` names the account that takes
every room file the imported database does not list; `--users` and `--owner` may be given
together, and at least one is required.

```sh
podman run --rm \
    --entrypoint /usr/local/bin/cocat-server \
    -v "$DEV/copy:/copy:Z" -v "$DEV/db:/db:Z" \
    129.104.6.172:32219/sciqlop/cocat-server-rs:devel \
    import --update-dir /copy/storage --db /db/cocat-rs.db \
    --users /copy/db/users.db --owner you@example.org --dry-run
```

Run the same command again without `--dry-run` once the summary looks right. It is
idempotent: a second run changes nothing.

## 3. Run the development server

The image already sets `COCAT_PROXY_PREFIX=""` and `COCAT_TRUSTED_PROXIES=1`; override them
for the dev deployment. `COCAT_PUBLIC_URL` is required as soon as a mailer is configured;
without one, registration and password reset have nothing to deliver.

| Variable | What the dev instance needs |
|---|---|
| `COCAT_PUBLIC_URL` | the address users reach the dev instance at, scheme and host only |
| `COCAT_PROXY_PREFIX` | the path the reverse proxy adds, empty when it is served at the root |
| `COCAT_TRUSTED_PROXIES` | how many proxies stand in front, `1` behind one |
| `COCAT_SMTP_HOST`, `COCAT_SMTP_PORT`, `COCAT_SMTP_USERNAME`, `COCAT_SMTP_PASSWORD`, `COCAT_SMTP_STARTTLS` | the relay, or leave them unset and use `COCAT_MAIL_DIR` with `COCAT_MAIL_FROM` |
| `COCAT_ROOM_MAX_MB`, `COCAT_OWNER_MAX_MB` | the room and per-owner storage limits, 1024 and 5120 by default |

```sh
podman run -d --name cocat-dev -p 6571:6570 \
    -v "$DEV/copy/storage:/storage:Z" -v "$DEV/db:/db:Z" -v "$DEV/log:/log:Z" \
    -e COCAT_PUBLIC_URL=https://cocat-dev.example.org \
    -e COCAT_PROXY_PREFIX=/cocat-dev \
    -e COCAT_TRUSTED_PROXIES=1 \
    -e COCAT_ROOM_MAX_MB=1024 -e COCAT_OWNER_MAX_MB=5120 \
    129.104.6.172:32219/sciqlop/cocat-server-rs:devel
```

The server keeps its database in `/db/cocat-rs.db` and its room files in `/storage`, which is
the copy, so it never touches the production volumes. `podman stop cocat-dev` flushes the
open rooms before it exits.

## 4. Check the dev instance

- `/docs` answers and the document loads.
- A Python account imported from the copy signs in; the rooms it saw on Python are listed
  with the same roles.
- SciQLop opens one room through the Python client and shows its catalogues and events.
- The same room's event count matches production.

## 5. Refresh the dev data

```sh
podman stop cocat-dev
podman rm cocat-dev
rm -rf "$DEV/copy" "$DEV/db/cocat-rs.db" "$DEV/db/cocat-rs.db.secret"
```

Then repeat steps 1 to 3. Starting over is quicker than importing on top and keeps the dev
instance honest.

## Switching production

Not done yet. When it is:

1. Stop the Python container.
2. Take the final copy and import it without `--dry-run`, exactly as in steps 1 and 2.
3. Start `:stable` on the production port with the production volumes.
4. Keep the Python volumes untouched until the Rust server has run for a while; verify logins,
   rooms and the daily use first.

Rollback is restarting the Python container. Room files are compatible in both directions, but
accounts created on the Rust server between the switch and the rollback are lost.
