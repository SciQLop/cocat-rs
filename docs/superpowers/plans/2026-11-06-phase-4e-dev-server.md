# Phase 4e — CI, container image and a dev server — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Run the Rust server as a dev instance next to the Python production server, on a
copy of production data, the way speasy-proxy runs `:devel` beside `:stable`; switch
production over only when the user decides it is ready.

**Architecture:** a GitHub Actions workflow runs the local gate. A two-stage container
image (Rust build stage, Fedora runtime) mirrors the Python image's layout (user `cocat`,
`/storage`, `/db`, `/log`, `PORT`, same wrapper commands), so the host's existing run
scripts need only an image name change. The image goes to the same registry as the Python
one under a new repository name, so the Python `:devel` and `:stable` tags are never
overwritten. A runbook covers seeding the dev instance from a copy of production with
`cocat-server import`.

**Tech Stack:** GitHub Actions (`dtolnay/rust-toolchain`, `Swatinem/rust-cache`), Podman/
Docker, `fedora:43` (same base as the Python image).

**Spec:** design phase 4 ("then the Rust server ships"); user decision 2026-11-06: first a
dev server, like the speasy proxy, production only when sure.

## Global Constraints

- This repo, branch `main`, commit per task, never push.
  ONE cargo or container build at a time, in the foreground.
- Registry and naming: `129.104.6.172:32219/sciqlop/cocat-server-rs:devel` (later
  `:stable`). Never push to `sciqlop/cocat-server` (the Python image).
- Image contract, same as the Python image (`cocat/docker/Dockerfile`): user `cocat`;
  `STORAGE_PATH=/storage`, `DB_PATH=/db`, `LOG_PATH=/log`, `PORT` (build arg, default
  6570), `COCAT_PROXY_PREFIX=""`, `COCAT_TRUSTED_PROXIES=1`; serves on `0.0.0.0:$PORT` with
  `--update_dir $STORAGE_PATH --db_path $DB_PATH/<name>.db`. The Rust database file is
  `cocat-rs.db`, never `users.db`, so a volume shared by mistake cannot mix the two schemas.
- Wrapper commands in the image with the Python wrappers' positional arguments:
  `create-user EMAIL PASSWORD`, `add-user-to-room EMAIL ROOM_ID`,
  `remove-user-from-room EMAIL ROOM_ID`.
- The build never needs the network at runtime of the container; the runtime stage holds
  only the binary, CA certificates (mail, OIDC) and the scripts.
- Nothing is deployed on the `sciqlop` host by an agent. The runbook says what to run; the
  user runs it or approves each step.

## Review Focus

1. **The Python data touched by the dev instance.** The runbook must copy the Python `.y`
   files and `users.db` before importing; the import opens `users.db` read-only, but the
   dev server writes to its `/storage`, which must never be the production directory.
2. **SIGTERM in the container.** The entry point must `exec` the server, or a stop waits
   for the timeout and kills it without the final flush.
3. **Proxy prefix and trusted proxies** behind the host's nginx: `/docs`, mailed links
   (`COCAT_PUBLIC_URL`) and the rate limiter's client address.
4. **A CI run that passes while the local gate would fail** (missing `-D warnings`, a
   different toolchain, `--workspace` left out).
5. **Image size and leftover build tree:** no `target/` in the runtime stage, a
   `.dockerignore` keeping `target/` out of the build context.

---

### Task 1: CI

**Files:** create `.github/workflows/ci.yml`.

- [ ] One job on `ubuntu-latest`, on push and pull request to `main`: checkout,
  `dtolnay/rust-toolchain@stable` with rustfmt and clippy, `Swatinem/rust-cache@v2`, then
  `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`. Same commands as the local gate (check them against the gate
  you run; if the gate differs, the workflow copies the gate).
- [ ] Validate the YAML locally (`python3 -c 'import yaml,sys; yaml.safe_load(open(sys.argv[1]))'
  .github/workflows/ci.yml`). It runs for real only after the user pushes. Commit "Run the
  gate in CI".

### Task 2: Container image

**Files:** create `docker/Dockerfile`, `docker/entry_point.sh`, `docker/create-user.sh`,
`docker/add-user-to-room.sh`, `docker/remove-user-from-room.sh`, `docker/build_and_push.sh`,
`.dockerignore`; README section "Container".

- [ ] `Dockerfile`: stage 1 `rust:1-bookworm` or `fedora:43` + rustup (pick the one whose
  glibc is not newer than the runtime's; `fedora:43` on both sides is the safe choice),
  `cargo build --release -p cocat-server --locked`. Stage 2 `fedora:43`, `ca-certificates`
  only, the contract above, `ENTRYPOINT ["/home/cocat/entry_point.sh"]`.
- [ ] `entry_point.sh`: `exec cocat-server serve --host 0.0.0.0 --port "$PORT"
  --update_dir "$STORAGE_PATH" --db_path "$DB_PATH/cocat-rs.db"`.
- [ ] Wrappers call the matching `cocat-server` subcommand with `--db_path
  "$DB_PATH/cocat-rs.db"`.
- [ ] `build_and_push.sh [testing|release]`: same shape as the Python script, tags `devel`
  or `stable` on `sciqlop/cocat-server-rs`, `--remote` inside a container. Only the build
  is run by the agent; the push is the user's.
- [ ] Smoke check (scratch dir, one build, foreground): build the image; run it with
  `-p 18570:6570` and scratch volumes; `create-user` through `podman exec`; `curl` shows
  `/openapi.json` 200 and a login 204 with the cookie; `podman stop` returns in well under
  10 s and the log shows the flush/shutdown line. Remove the container and image after.
  Paste the commands and outputs in the commit message body or the report. Commit "Build
  a container image".

### Task 3: Dev-server runbook

**Files:** create `docs/deploy.md`; link it from the README.

- [ ] Steps, each a command block:
  1. Copy production: the Python `/storage` (`.y` files) and `/db/users.db` into new
     directories for the dev instance (a copy, never a bind of the production volumes).
  2. Import: `cocat-server import --update-dir <copy>/storage --db <dev>/db/cocat-rs.db
     --users <copy>/db/users.db [--owner ...] --dry-run`, read the summary, then without
     `--dry-run`. The flags are the ones `cli.rs` defines; check them while writing.
  3. Run the `:devel` image on the dev port with the copied storage and the new db, and
     the environment: `COCAT_PROXY_PREFIX`, `COCAT_TRUSTED_PROXIES`, `COCAT_PUBLIC_URL`,
     mailer variables, `COCAT_ROOM_MAX_MB`/`COCAT_OWNER_MAX_MB` (README table).
  4. Check: `/docs`, a login, one room opened from SciQLop with the Python client, and
     the same room's event count against production.
  5. Refresh the dev data later: stop, delete the dev copy, repeat 1–3.
- [ ] A short "Switching production" section, marked as not done yet: stop Python, final
  copy + import, start `:stable` on the production port, keep the Python volumes untouched
  until verified; rollback is restarting the Python container (room files are compatible
  both ways, accounts created on Rust in between are lost).
- [ ] Commit "Document the dev server and the switch-over".
