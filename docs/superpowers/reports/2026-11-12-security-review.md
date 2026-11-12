# Security, privacy and durability review — 2026-11-12

Scope: cocat-rs `fd05753..bc10cad` (everything after the phase 4b campaign), plus the
cocat-dev host. Read-only review by three parallel reviewers (access control, data leaks,
storage); the main findings below were re-checked against the code. Nothing is fixed yet.

## Fix first

| # | Area | Finding | Where | Severity | Checked |
|---|---|---|---|---|---|
| 1 | storage | A bad length byte in the middle of a project file is taken for a cut-off tail: the file is truncated in place and everything after it is lost, with only a "ends mid-record" warning. | `cocat-core/src/store.rs:267-298`, `:76-79` | high | yes |
| 2 | host | No backup of any kind on the sciqlop host (no cron, no timer, no ZFS snapshot), for production or dev. | host | high | yes |
| 3 | storage | A missing database is created empty without a word (wrong path, unmounted volume): every project loses its owners. | `db.rs:70` | high | yes |
| 4 | leak | A submission carries the author's trashed events; once accepted they are public. Round candidates purge the trash, submissions do not. | `submissions_api.rs:592-599`, `round_candidates.rs:74` | medium/high | yes |
| 5 | auth | `PATCH /users/me` changes email or password without the current password; with the device sign-in flow, a phished "Allow" becomes an account takeover. | `accounts_api.rs:224` | medium | yes |
| 6 | storage | A failed write (disk full) is only logged; the project keeps accepting edits, which are lost when it unloads. | `room.rs:1423` | medium/high | write path yes, unload loss not re-checked |
| 7 | auth | Logout and password change delete the session but leave its open websockets alive (token revoke and reset do close them). | `auth.rs:198`, `accounts.rs:219` | medium | yes |
| 8 | leak | API sign-up says whether an email is registered; forgot-password / verify answer slower for known addresses (the web pages avoid both). | `accounts_api.rs:76`, `:108`, `:126` | medium | yes |
| 9 | storage | No lock on the data folder: two servers, or `import` (even `--dry-run`) on live files, can truncate each other's records. | `cocat-core/src/store.rs:76`, `import.rs:169` | medium | from report |
| 10 | host | Data files and the SQLite db (password hashes) are world-readable (644/755); a stale copy of production's `users.db` sits in `/DATA/cocat_rs_dev/copy/`. | host | medium | yes |

## Then

- Copy chains keep earlier hops' **project names and ids** (emails are dropped on public
  catalogues by design, ruling of 2026-11-10); private copies keep every hop's email. (leaks #3-4)
- Websocket handshake has no Origin check (cookie is SameSite=Lax, so only sibling subdomains). (access #3)
- Open redirect after sign-in (`/.//evil.com`), **only on a deployment without URL prefix**; both LPP
  deployments have one. (access, `ui/login.rs:172`)
- Uploaded files: no fsync before rename, hash not checked when served, a bad blob cannot be
  re-uploaded; downloads lack `nosniff` / `Content-Disposition: attachment`; dropped files stay
  downloadable until the project is deleted.
- Submission snapshots are never deleted; a curator's review note becomes the public version message;
  blind rounds show who started a proposal.
- App tokens have no scope (a token is the whole account, superuser included); no anti-framing header.
- Relay retry trusts `Retry-After` without a cap and reads bodies without a size limit.
- Metrics keep one series per raw HTTP method (unbounded).
- No `Cache-Control` on private responses; Python client keeps its local copy after access is removed.

## Fine (suspected and ruled out)

Cross-project ids (documents, rounds, submissions, blobs) are all filtered by project; blind rounds
hide what they should; relayed catalogues are read-only; admin is superuser-only; CSRF covers every
non-GET route; no stored XSS (notebook markdown sanitized, HTML outputs sandboxed); links grant
nothing; role changes close sockets; presence silent in public projects; author uuids no longer in
public history JSON (old nit fixed); no secrets in logs; cookie `Secure; HttpOnly; SameSite=Lax`;
torn stamps, torn tails and bad updates are handled; relay is not an SSRF (provider URLs are fixed).

## Durability in one table

| Failure | Detected? | Lost |
|---|---|---|
| process killed | — | nothing (written before broadcast) |
| power loss | — | ≤ ~1 s of edits; clients resend on reconnect |
| torn last record | warning | that record; clients resend |
| bad byte mid `.y` | misleading warning | **everything after it, file truncated in place** |
| `.y.h` stamps lost | handled | who/when only |
| blob missing / corrupt | 500 / **not detected** | that file |
| SQLite lost | **not detected, recreated empty** | accounts, memberships, groups, tokens, versions, rounds, submissions, file index; `import --owner` recovers catalogue projects only |
| disk full | log only | edits since, at unload |

Detailed reports (file:line for everything): `2026-11-12-review-{access,leaks,storage}.md` next to this file.
