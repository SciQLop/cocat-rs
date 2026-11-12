# cocat-rs storage durability and corruption review

Repo: cocat-rs at bc10cad. Read-only review, nothing built or run.
Paths below are relative to `crates/cocat-server/src/` unless they start with `cocat-core/` (meaning `crates/cocat-core/src/`).

## Short answer

Ordinary crashes are handled well. A killed process loses almost nothing. A torn last record is cut off and the project opens.

The weak spot is anything that isn't a torn tail. The project files have no checksums. One bad length byte in the middle of a `.y` file makes the loader treat everything after it as a torn tail. It then **cuts the file at that point, in place, with no copy kept**. Only a warning goes to the log.

There is no backup command and no lock file. A full disk is only logged: edits stay in memory and are lost when the project unloads.

---

## 1. What is on disk

| File | Where | Format | Holds | Written by |
|---|---|---|---|---|
| `<room>.y` | `--update_dir` (Docker: `/storage`) | header `0.0.1\0`, then records of `varuint(len)` + Yjs update v1. No checksum. | every edit of a catalogues project, forever | `cocat-core/store.rs:117-150` (append/write), `:153-160` (sync), `:260-265` (reset), `:73-79` (truncate on open) |
| `<project>%2F<doc-uuid>.y` | same | same | one workspace document (manifest, notebook, text) or one round proposal | same code. Key is `documents.rs:22-24`, escaping `/`→`%2F` is `cocat-core/store.rs:186-229` |
| `<room>.y.h` | same | fixed 48-byte entries: record index, user, time, length, restore mark, xxh3 hash of the record | who wrote each record and when (history authorship) | `history/stamps.rs:94-153`, file name `:85-89` |
| `blobs/<project>/<sha256>` | `update_dir/blobs/` | raw bytes, named by SHA-256 | a workspace's uploaded whole files | `blobs.rs:87-99` (write temp file, then rename) |
| `blobs/<project>/.<uuid>.tmp` | same | partial blob | left only if the process dies between write and rename | `blobs.rs:89` |
| `cocat-rs.db` | `--db_path` (Docker: `/db/cocat-rs.db`) | SQLite. sqlx 0.9 defaults: rollback journal (DELETE mode), `synchronous=FULL`, 5 s busy timeout. The code sets only `create_if_missing` and `foreign_keys` (`db.rs:67-75`). | everything that is not document content (see the list below) | sqlx everywhere. Migrations in `crates/cocat-server/migrations/` |
| `cocat-rs.db-journal` | next to the db | SQLite rollback journal | exists only during a write transaction. After a crash it is a "hot journal" and must not be deleted. | SQLite |
| `cocat-rs.db.secret` | next to the db | text, 64 hex chars | signing secret for mailed links and the provider sign-in cookie (only if `COCAT_SECRET` is unset) | `auth.rs:124-145`: temp file, `sync_all`, hard link. Path built at `auth.rs:453-457` |
| history "cache" | **memory only** | — | grouped versions, recent diffs | `history/cache.rs:18`, `:93`. Nothing on disk. |

What the database holds that the `.y` files do not:
- users, password hashes, superuser and curator flags, quotas (`quota_mb`, `limit_mb`), personal room link
- projects (name, kind, public flag, folder, description, `editors_can_share`, public counts)
- memberships, groups, group members, group-to-project roles, invitations
- sessions and API tokens, sign-in requests, OAuth links
- the list of a workspace's documents (`document` table) and the blob index with sizes (`blob` table)
- saved versions and version names (`version_commit`, `version_name`). They point at **record indices** in the `.y` file.
- submissions to public catalogues. Their `snapshot` column is the **only copy** of the submitted catalogue (`migrations/0016_submissions.sql`).
- proposals, review rounds, votes, verdicts, round log
- relay settings and state, and which public catalogue each relay feeds (`migrations/0023_relay.sql`)

**docs/deploy.md vs the code.**
- The mount table (`docs/deploy.md:42-43`) gets the files right, including `%2F` for documents and `blobs/`.
- It says the db holds "accounts, projects, memberships, sessions". That undersells it. Submissions (the only copy of submitted data), rounds, saved versions, groups, relay state and the blob index also live only there.
- The backup section (`docs/deploy.md:291-309`) is correct: online `.backup` for the db, a live copy of `.y` files loses at most the last edits.
- It doesn't say which order to copy in (see section 4).
- It doesn't warn that a corrupt record in the middle of a file gets truncated away.

---

## 2. The write path

How an edit travels, step by step:

1. A frame arrives on the room's own thread. The quota is checked **before** the edit is applied (`room.rs:1683-1697`). A refused edit changes nothing.
2. The edit is applied to the in-memory document. Undecodable records and yrs panics are caught and skipped (`room.rs:1748-1761`, `:1567`).
3. The record is added to an in-memory `pending` buffer (`cocat-core/store.rs:117-122`). A stamp is queued the same way (`room.rs:1702-1707`).
4. After the command, if no other command is queued, pending records are written with `write_all` on an `O_APPEND` handle (`room.rs:1149-1151`, `cocat-core/store.rs:126-150`). Stamps are written only if the room write succeeded (`room.rs:1423-1431`).
5. Then the update is broadcast to the other clients (`room.rs:1152-1154`). REST writes (`write_as`) are answered at `room.rs:1158-1160`.
6. `sync_data` runs on a 1-second idle timer (`room.rs:1029-1033`, `:1433-1442`). It also runs on unload, flush and shutdown (`room.rs:1192`, `:1444-1447`, `cli.rs:405`).

What this gives:
- **Process crash or `kill -9`:** almost nothing is lost. Written data sits in the kernel page cache, which survives the process.
- **Power loss:** about one second of edits is lost, as documented (`room.rs:36-39`).
- **Exception:** in step 4, the write is skipped when more commands are already queued. The broadcast in step 5 still happens. So under load an update can reach other clients before it is written. A crash in that window loses an update that others have already seen. The window is tiny. But the comment at `room.rs:36-37` and `:1147-1148` claims "every update is written before it is broadcast", which isn't strictly true.
- **No acknowledgement:** the Yjs protocol has no per-update ack, so "acknowledged" here means "broadcast".

**Partial write.** `write()` records the file length first. On error it cuts the file back to that length and keeps the records pending for a retry (`cocat-core/store.rs:130-146`). Stamps do the same (`history/stamps.rs:132-144`). Good.

**Atomic rename.** Only blobs and the secret use it (`blobs.rs:87-99`, `auth.rs:131-145`). Nothing rewrites a `.y` or `.y.h` file. There is **no compaction at all**, despite the comment at `public_api.rs:281` ("between compactions").
- The blob path never fsyncs, neither the file before the rename nor the directory after.
- New `.y` and `.y.h` files are created without an fsync of the directory.

**Torn last record on load** (`cocat-core/store.rs:56-90`, `:267-298`):
- The parser reads records until a length prefix doesn't fit, is zero, or the varint is broken. That point is called the "truncation offset".
- `open` then calls `set_len(offset)`, which cuts the file there for good.
- A file that is only a prefix of the header is reset to the header.
- A wrong header gives `BadHeader`. The room thread logs it and abandons the room (`room.rs:960-966`, `:983-1004`). Clients get close code 1011 (`Failed`).
- Read-only paths (`read_updates`, `cocat-core/store.rs:240-250`) skip the torn tail without writing. The next real open cuts it.

**Bad records inside the file.** Each record is applied inside `catch_unwind` and skipped if it fails (`room.rs:1748-1820`, test at `room.rs:1954`). The room still opens.

**A panic while serving** (`room.rs:1036`, `:1166-1175`):
- The room closes every client with 1011 and unloads.
- It does **not** flush `pending`. Records still waiting (only possible when commands were queued, see above) are dropped.

**Healing on reconnect.** On attach the server sends its state vector (SyncStep1, `room.rs:1535-1537`). A client that still holds edits the server lost pushes them back. This is the real safety net for every "lost the tail" case below. It only works while a client still has that state (an open browser tab, or a SciQLop client's local cache: `cocat-client/src/room.rs:496`).

---

## 3. Corruption scenarios

| Failure | Detected? | What the user sees | What is lost | Hand repair |
|---|---|---|---|---|
| Torn last record of `.y` (crash or power loss mid-append) | yes, `warn` log "ends mid-record" (`cocat-core/store.rs:65-71`) | nothing. The project opens. | the last partial record, plus up to ~1 s of unsynced edits on power loss. Connected clients push them back. | none needed |
| **Bad length byte in the middle of `.y`** | only as the same misleading "ends mid-record" `warn` | the project opens, missing every edit after the bad byte (or with garbage records skipped, then cut) | **everything after the bad byte. The file is cut in place on the next open** (`cocat-core/store.rs:76-79`) and no copy is kept. Saved-version names now point at the wrong records (record indices are reused). | only from a backup, or from a client that still holds the full state |
| Bad payload byte in the middle (lengths intact) | yes if yrs rejects it (`warn`, skipped). Not detected if it still decodes. | missing or wrong content. Later edits that depend on the bad one stay pending in yrs. | that record, and edits that build on it | The stamps file has an xxh3 hash per record (`history/stamps.rs:69-72`) that could find the bad record, but the loader never uses it |
| Bad header | yes, `error` log | close code 1011. The project won't open. History and REST reads fail. | nothing on disk | fix the first 6 bytes (`0.0.1\0`) |
| v1 layout file | yes (`room.rs:969-972`) | 1011 | nothing | run `import` |
| `.y.h` missing | no (recreated empty) | history shows old records as one unattributed version (`history/versions.rs:541`) | authors and times. Content is intact. | none |
| `.y.h` corrupt | per entry: an entry whose hash or length doesn't match its record is ignored (`history/stamps.rs:171-184`). A torn tail is cut on open (`:104-120`). | as above, for the bad entries | authors and times only | none |
| "History cache" corrupt | n/a, it is memory only | — | — | restart |
| Blob missing | at read: 500 + `error` log (`blobs.rs:220-223`) | download fails | the file | Put the file back by hand. Re-uploading through the API **does not repair it**: `put_blob` sees the db row and returns 200 without writing (`blobs.rs:173-175`). |
| Blob corrupt or zero-length (e.g. power loss after rename without fsync) | **no**. The hash is not checked on read (`blobs.rs:219-225`). | wrong bytes, served as if fine | the file | same as missing: replace the file by hand |
| SQLite corrupt | at startup, only if SQLite or a migration notices. Otherwise query errors at runtime. | server refuses to start (`cli.rs:362`), or 500s | depends on the damage | `sqlite3 .recover`, or restore a backup |
| **SQLite lost or wrong path** | **no**. `create_if_missing(true)` (`db.rs:70`) silently makes a fresh empty db. | everyone is "unknown". The projects vanish from everyone's list. | users, password hashes, memberships, groups, sessions, tokens, quotas, public flags, relay mapping, workspace document lists, blob index, saved versions, **submission snapshots (only copy)**, rounds | `import --owner <email>` re-adopts the catalogue `.y` files under one owner (`import.rs:446-503`). The room name becomes the id. Workspace documents are skipped (`import.rs:458-463`), so workspaces come back broken. Everything else in the db list is gone. |
| Secret file lost | no. A new one is made silently (`auth.rs:107-114`). | mailed links and provider sign-in cookies in flight stop working | nothing durable | none needed |
| Disk full during append | `warn` log only (`room.rs:1424-1427`). The file is cut back cleanly (`cocat-core/store.rs:135-146`). | **nothing**. Edits are still broadcast and the room keeps serving. | Edits stay in memory and are retried on every write. When the room unloads, the final flush fails again (`room.rs:1192`) and they are **dropped**. Clients that still hold them push them back on reconnect. | free disk space while the room is still open |
| Two server processes on one data dir | **no lock file** (no `flock` or lockfile anywhere) | each process has its own in-memory doc, so users of one don't see edits made through the other | Both append to the same files (O_APPEND keeps whole writes intact). But the stamp record indices diverge, so authorship breaks. One process's truncate-on-open or partial-write rollback (`set_len`) can **cut records the other just wrote**. | stop one. The CRDT content usually merges back on the next open, unless a `set_len` hit. |
| `import` (even `--dry-run`) run against live room files | no | — | `FileRoomStore::open` truncates torn tails before any dry-run check (`import.rs:169`, `:465`). If the Python server or the Rust server is mid-append, this cuts its record, and a later misaligned tail is cut again. | Docs say to run it on a copy first (`docs/deploy.md:262`). That is the only guard. |

---

## 4. Backups and recovery

- **No backup, export or snapshot command.** The CLI has `create-user`, `create-room`, `add-user-to-room`, `remove-user-from-room`, `set-room-owner`, `set-superuser`, `set-curator`, `import` and `serve` (`cli.rs:37-133`). Backup is left to the operator (`docs/deploy.md:291-309`).
- **A live file copy of `.y`/`.y.h` is consistent enough.** The files only grow, and a copy ending mid-record is cut cleanly on open. A copy taken between two `pending` writes just misses the newest edits.
- **The SQLite db must be copied with `.backup`, as the docs say.** With the rollback journal, a plain copy taken during a write can be torn.
- **Order matters, and the docs don't say it.** Every disk file is written *before* its db row: blob file then row (`blobs.rs:181-187`); project row created, file appears on first edit. So take the **db backup first, then copy `/storage`**. The other order can leave db rows (blobs, documents) whose files aren't in the copy, which gives 500s after restore.
- **Rebuilding from `.y` files:** `import --owner` (`import.rs:113-138`, `:446-503`) is the only path. It brings back catalogue projects with one owner and their id as name. Workspaces, sharing and everything db-only (see section 1) are not rebuilt.
- **History is a recovery tool for bad edits, not for disk loss.**
  - Restore (`history_api.rs:716-764`) replays the log up to a version and writes one new edit that makes the project equal to it again. This works well against a bad edit or a bad import.
  - It lives in the same `.y` file, so it gives no protection against losing or corrupting that file.
  - A restore is a write. It is refused with 409 when the project is at its storage limit (`history_api.rs:712`). So you can't undo a bad edit on a full project without raising the quota first.
  - Deleting a project removes its files at once, with no trash (`room.rs:411-417`, `:1372-1387`, `blobs.rs:53-62`). History can't bring a deleted project back.

---

## 5. Limits and growth

- **Limits** (`room.rs:79-97`, env vars read at `cli.rs:426-451`):
  - `COCAT_ROOM_MAX_MB`: 1 GiB per room key, `.y` + `.y.h` counted (`room.rs:549-554`)
  - `COCAT_OWNER_MAX_MB`: 5 GiB per owner, which also counts blobs and submission snapshots (`room.rs:346-389`)
  - `COCAT_FILE_MAX_MB`: 20 MiB per blob (`blobs.rs:167`)
  - 50 MiB per submission snapshot (`submissions_api.rs:57`)
  - per-account and per-project overrides in the db
- **Hitting a limit mid-write is clean.** The check runs before apply and append (`room.rs:1683-1697`). The socket closes with 4507. The edit stays only on that client. It is re-sent and refused again on every reconnect until space is freed.
- **Known overshoot:**
  - The allowance is measured when a room opens. Two rooms of one owner open at once can each spend the remaining quota (`room.rs:326-330`).
  - Concurrent blob uploads can each overshoot by one file (`blobs.rs:177-178`).
  - Both are marked `simplify:` in the code.
- **Relay rooms have no owner**, so only the room limit applies (`room.rs:337-339`, `relay/mod.rs:396`). Each run that changes something appends a full diff. A busy feed grows toward 1 GiB and then every run fails.
- **Unbounded growth:**
  - `.y` files are never compacted. Every edit is kept forever, and the memory needed to open a room is estimated at 16× the file size (`room.rs:45-47`).
  - The stamps file grows 48 B per record (counted in quota).
  - Blobs are never swept until the project is deleted (`blobs.rs:4-6`, counted in quota).
  - Decided submissions keep their snapshot in the db forever (`submissions.rs:122-125`, counted against the author).
  - The in-memory `VersionCache` keeps an entry per room ever viewed (`history/cache.rs:15-17`).
  - Expired sessions are deleted only per user on that user's next login (`db.rs:720`).
- **The "at most one second" fsync bound is not strict.** The fsync timer fires only when the command channel is idle (`room.rs:1282-1298`, `:1029-1033`). A room that always has a command queued (bulk import, a flood of frames) delays its fsync until the queue drains.

---

## Confirmed issues, ranked

1. **High: corruption in the middle of a `.y` file silently and permanently cuts everything after it.** `cocat-core/store.rs:267-298` can't tell a torn tail from a bad length in the middle. `cocat-core/store.rs:76-79` then truncates in place. No `.corrupt` copy is kept and the log message (`:65-71`) says "ends mid-record". Fix: only truncate when the bad spot is within the last few KB or the last record, otherwise refuse the room and keep the file. Or copy the cut tail to `<room>.y.tail-<ts>` before truncating. The stamps' per-record xxh3 could also be used to check records.
2. **High: a missing db is silently recreated empty** (`db.rs:70`, `cli.rs:362`). A wrong `--db_path`, or a volume that isn't mounted, starts a server where every project has lost its owners. New signups then write into that new db. Fix: `create_if_missing` only on first setup, or refuse to start when `update_dir` holds `.y` files but the db is new.
3. **Medium-high: disk-full and other write errors are only logged.** `room.rs:1423-1431` warns and the room keeps accepting and broadcasting. The final flush on unload fails and drops the pending records (`room.rs:1192`, then the store is dropped). There is no metric, no client error, no refusal. Fix: on write failure, stop accepting updates (close with 1011/1013) and keep the room loaded while it retries. Expose a telemetry counter.
4. **Medium: no lock on the data dir.** Two servers, or `import` while the server runs, can `set_len` away the other's records (`cocat-core/store.rs:76-79`, `:138`, `history/stamps.rs:112`, `:138`). Fix: take an `flock` on `update_dir/.lock` in `serve` and `import`.
5. **Medium: blobs aren't fsynced before rename, and are never checked on read.** A blob can be zero-length after a power loss and is served as-is. Re-uploading can't repair it because `put_blob` trusts the db row (`blobs.rs:87-99`, `:173-175`, `:219-225`). Fix: `sync_all` before rename. In `put_blob`, rewrite when the file is missing or its size is wrong. Optionally verify the hash on read.
6. **Medium-low: `import --dry-run` writes.** `FileRoomStore::open` truncates torn tails and resets short files before the dry-run check (`import.rs:169`, `:465`). Use `read_updates` for the checks.
7. **Low: a panic while serving drops pending records without a flush** (`room.rs:1166-1175`). It only matters when commands were queued.
8. **Low: "written before broadcast" doesn't hold under load** (`room.rs:1149-1154` vs the comment at `:36-37`, `:1147-1148`). The same queued case lets a broadcast go out before its write.
9. **Low: the fsync timer can be starved by a constantly busy room** (`room.rs:1282-1298`).
10. **Low: no directory fsync** after creating a new `.y`/`.y.h` or blob. A brand-new project can lose its file entry on power loss.
11. **Low (docs): deploy.md undersells what the db holds and gives no backup order.** The db holds submissions, rounds, saved versions and relay state. The safe order is db `.backup` first, then `/storage`. There is also a stale comment about "compactions" at `public_api.rs:281`.

## Observations (not bugs)

- Saved-version names and commits are record indices (`migrations/0008`, `0009`). After any truncation they point at whichever new record took that index.
- Deleting a project is immediate and final, files included.
- At the storage limit, history restore is refused. Raise the quota first.
- An edit refused at the limit lives only in that client, which keeps retrying.

## False positives (suspected, checked, fine)

- *Torn stamps break alignment*: no. `StampFile::open` cuts to whole 48-byte entries (`history/stamps.rs:104-120`). Every stamp is matched to its record by index plus xxh3 hash (`:171-184`), so stale or misaligned stamps are ignored.
- *A yrs panic on a bad record takes down the room forever*: no. `catch_unwind` around apply (`room.rs:1748-1761`) and around frame decode (`:1567`). Tested at `room.rs:1954`.
- *A partial write leaves half a record*: no. It is rolled back with `set_len(start)` (`cocat-core/store.rs:135-146`).
- *Cold reads truncate the file*: no. `read_updates` is read-only (`cocat-core/store.rs:238-250`).
- *A quota hit mid-write leaves a partial state*: no. The check runs before apply and append (`room.rs:1683-1697`).
- *Process kill loses up to a second*: no. Writes happen right away. The 1 s fsync only matters for power or OS loss.
- *SQLite durability*: sqlx 0.9 keeps SQLite's defaults, which are rollback journal + `synchronous=FULL` (`~/.cargo/registry/.../sqlx-sqlite-0.9.0/src/options/mod.rs:179-191`). Durable per commit.
- *Secret file torn on creation*: no. Temp file + `sync_all` + hard link (`auth.rs:131-145`).
- *The history cache can be corrupted on disk*: it is memory only.

Not sure about: whether axum's graceful shutdown waits for upgraded websocket connections before `flush_all` (`cli.rs:403-405`). Either way, each room flushes when its last client leaves, and written data survives the process exit.
