# cocat-rs: privacy and data-leak review (fd05753..HEAD)

Read-only review. Nothing was built or run, and no server was contacted. Every finding below
comes from following the real code path, but none has a reproducer yet. Paths are relative
to `crates`.

Who can read what (the access model all findings rely on):
- Every signed-in account is a **viewer of every public catalogue**. Curators and superusers are
  editors there. See `cocat-server/src/db.rs:244` (`role_in`, last `UNION`).
- Any viewer can open the websocket, `GET /api/rooms/{id}/state`, and the full history
  (`/history`, `/history/{v}/state`, `/history/{v}/copy`). All of these check only `membership`.

---

## Ranked findings

### 1. A submission publishes the author's trashed events — Medium/High

What happens, in order:
1. Alice moves some events of her catalogue to the trash. Trashing only sets `status = deleted`. The
   event stays listed in the catalogue (`cocat-core/src/model/write.rs:198`).
2. She submits the catalogue. `snapshot_of` → `extract_alone` → `model::extract`
   (`cocat-server/src/submissions_api.rs:564`, `:597-599`).
3. `extract` calls `restore(.., Some(catalogue))`, which copies "the events a catalogue lists,
   trashed ones included" (`cocat-core/src/model/restore.rs:106-107`).
4. A curator accepts it. The snapshot is written into the new public room (`new_public`), or
   adopted into the existing one (`adopt_into`, for an update submission).
5. Every signed-in account can now read those trashed events, over the websocket, `/state`, the
   web trash view, and history.

Who sees it: every signed-in account. Before acceptance, the curators also see it in the
submission diff.

The code shows the intended pattern elsewhere. Round candidates purge the trash explicitly before
copying (`cocat-server/src/round_candidates.rs:74-80`, `live_copy`). Submissions do not. No test in
`tests/submissions.rs` checks this.

The same applies to update submissions. Events the author drafted in their copy and then trashed
land in the public catalogue's trash.

Fix: in `snapshot_of`, purge the trash the same way `live_copy` does.

### 2. Account enumeration through the JSON API — Medium

The UI sign-up hides whether an address is taken when a mailer is configured
(`ui/signup.rs:65-70`). The JSON API does not:

- `POST /auth/register` answers `400 REGISTER_USER_ALREADY_EXISTS` for a taken address
  (`accounts_api.rs:76`, `api.rs:133`). It needs no session, only the login rate limit.
- Timing oracle: `POST /auth/forgot-password` and `POST /auth/request-verify-token` **await the
  SMTP send inline** (`accounts_api.rs:108`, `:126`). A known address costs an SMTP round trip.
  An unknown one returns at once. The UI runs the same calls with `tokio::spawn`
  (`ui/reset.rs:77`, `ui/login.rs:134`) precisely to avoid this.
- `/auth/register` for a new address also awaits the verification mail.

`PATCH /users/me` answering `UPDATE_USER_EMAIL_ALREADY_EXISTS` (`api.rs:142`) is a smaller
version of the same leak. It needs a session and is rate-limited. Low.

Fix: spawn the mail in the API handlers too. Decide whether `/auth/register` keeps the
fastapi-users error code for compatibility, or answers 201-alike plus the notice mail, as the UI
does.

### 3. A public catalogue names its submitter's email, and earlier private project names — Medium

What happens:
1. A new submission records a copy hop with `by = submitter email` (`submissions_api.rs:271`).
2. On acceptance, `keep_only_last_author` (`copy_chain.rs:38`, called through `without_copiers`
   at `submissions_api.rs:878`) removes `by` from the earlier hops only.
3. It keeps every hop's `project` id, `project_name`, `catalogue_name` and date.

Result: every signed-in account sees:
- the submitter's email, in the catalogue's `copied_from`. The web app shows it as "Copied, oldest
  first … by <email>" (`cocat-web/src/rule_text.rs:298-323`).
- the **names and ids of the private projects** the catalogue passed through before. For example:
  "Unpublished Juno shocks › draft v3".

This contradicts two other places that try to hide authors in public catalogues:
- history: "never an email", and author uuids are hidden (`history_api.rs:920`, `:966`, `:885`).
- presence: it is silent in public rooms "so nobody's email is shown there" (`presence.rs:68`).

The comment in `copy_chain.rs` says naming the submitter is intended. The leak of earlier private
project names is very likely not intended.

Fix: for a public catalogue, drop or blank `project_name` and `catalogue_name` of every hop
before the last. Then decide once whether the submitter is named. Today history says no and the
chain says yes.

### 4. Private copy chains carry other people's emails and project names — Low/Medium

`record_copy` (`copy_chain.rs:20`) appends a hop and keeps the **whole earlier chain**,
emails included. This happens on every server-side copy:
- history copy (`history_api.rs:398`)
- round candidates (`round_candidates.rs:37`, through `recorded_copy`)
- copying a public catalogue (`submissions_api.rs` `public_copy`)
- freezing a smart catalogue, done by the client

Example:
1. Alice copies a catalogue from P0 into her project P1.
2. Carol copies it from P1 into P2, which she shares with Dave.
3. Dave sees "P0 › X, by alice@…" and "P1 › X, by carol@…".
4. Dave was never a member of P0 or P1.

For round candidates, every member of the round's project sees the name of the source project.
Only the round creator had access to it.

These emails live in the CRDT document and in its immutable history. Deleting an account does not
remove them, so this is a right-to-erasure problem as well.

Severity is low to medium because the chain is a deliberate feature. It still matches "a
project's name leaking through a copy chain to people with no access" exactly.

### 5. Submission snapshots outlive the catalogue, the project and the withdrawal — Low

- `submission.room` is `ON DELETE SET NULL` (`migrations/0017_submission_room.sql:3`).
- Nothing ever deletes submission rows or clears `snapshot`. Searching for `DELETE FROM submission`
  finds nothing.

So once a catalogue has been submitted, a full copy of it stays forever. Its author and **every
curator** can read it (`GET /api/public/submissions/{id}`, which includes the full diff). This
holds after the author withdraws it, after a decline, and after the source catalogue or project is
deleted. The snapshot is deleted only when the author's account is deleted (the author column
cascades).

### 6. Uploaded files are never removed until the project is deleted — Low

- `blob` rows and files are removed only by `delete_project` (`blobs.rs`; `room.rs:411`).
- Removing a file from the manifest leaves it in place.
- Any member, viewers included, can still `GET /api/rooms/{id}/blobs/{sha256}` (`blobs.rs:207`).
  The hash stays in the manifest's history.
- The bytes also keep counting against the owner's quota.

### 7. Purged or deleted data stays readable through history — Low (by design, worth stating)

`purge_event` says "Removes the event for good" (`cocat-core/src/model/write.rs:207`). Yet any
member, viewers included, can replay it through:
- `GET /history/{v}/state` (`history_api.rs:321`)
- `POST /history/{v}/copy`, which needs `membership` only (`history_api.rs:398`)
- the diffs

For a public catalogue this means every signed-in account. A curator who purges personal data that
was submitted by mistake cannot really remove it. Only deleting the whole public room removes it:
it deletes the log and the stamps, and forgets the caches (`room.rs:439-441`, stamps at
`room.rs:1371`).

The live websocket and `/state` endpoints do not resend purged content. yrs GC is on, since no
`skip_gc` is set anywhere. I am fairly confident of this, but did not verify it by running.

### 8. A curator's review note becomes public — Low

On acceptance, `take_in` saves the version with the review `note` as its commit message
(`submissions_api.rs:795`). The note is addressed to the author: it appears as `review_note` in
`SubmissionOut`. Every reader of the public catalogue's history sees it as `message`.

### 9. Blind rounds show who has started a proposal — Low

`present()` lists `proposals[].author` as an email for every proposal document, whatever the
visibility and state (`rounds_api.rs:280`). Only `doc` is hidden. While a blind round is
proposing, every project member learns who has proposed. Whether participation is meant to be
secret is a product question.

`Emails::of` falls back to the raw user uuid for a deleted account (`rounds_api.rs:237`). This is
trivial.

### 10. No `Cache-Control` on private API responses — Info/Low

Only the `/ui/app` index sends `no-store`. All of these are cookie-authenticated private data sent
without any cache directive:
- the JSON API, `/api/rooms/{id}/state`
- `/history/{v}/state`
- blobs
- the askama UI pages

With no validator, heuristic caching is unlikely in practice. Still, a `no-store` (or `private`)
on `/api/*` and `/ui/*` costs nothing. No Referrer-Policy or frame-ancestors is set either. The
device sign-in code is in the query string `/ui/connect?code=`, and browsers by default send only
the origin cross-site. SameSite=Lax keeps the session cookie out of a cross-site iframe, so
framing `/ui/connect` does not work.

### 11. The Python client keeps a full local copy after access ends — Info

`platformdirs.user_cache_dir("cocat_rs")/<server>/<account>/` (`cocat-py/python/cocat_rs/client.py:476`)
holds the room file. Nothing prunes it when the member is removed or the project is deleted. This
is inherent to an offline client. It is worth a line in the docs and maybe a `forget(project)`
helper.

### 12. Relay list URLs are not masked — latent, Info

`RelayOut.url` is shown to every signed-in account (`public_api.rs` `out`). It is not passed
through the secret masking that run reports get (`relay/mod.rs` `hide`). Today no provider
declares settings, and every list URL is a public page (DONKI, DOI, PMOD…), so nothing leaks.
If a future provider puts a key in the list URL, it would leak.

---

## Checked and fine (false positives)

- **Known nit, author uuids in public history JSON: fixed.** `views` sets
  `anonymous = is_public && !role.can_write()` (`history_api.rs:885`). In that case
  `author_unless_anonymous` clears `version.author` (`:966`), and emails become "a curator" / "a
  contributor". Residue: versions still group consecutive records by author, so a reader can tell
  version boundaries between authors apart. Not exploitable for identity.
- Presence in public rooms relays nothing (`presence.rs:68`). Its `user` is forced to the
  authenticated email and catalogue rooms keep only `editing`. Presence is scoped per room key, so
  a blind proposal's awareness never reaches non-readers.
- Event comments (a separate `comments` root) and origins are not copied by `extract`, so private
  notes do not reach submissions or public catalogues.
- Links between projects can be stored only in the owner's personal project. The server never
  resolves them. The Python client's `open_link` needs membership.
- Version and diff caches are forgotten on room deletion (`room.rs:440-441`), and stamps are
  deleted with the room file. A reused `public.<slug>` id starts clean.
- Private project existence: `membership` gives a uniform `404 Project … not found`, and the
  websocket gives 403 for both unknown and forbidden. Blobs, history, submissions, round
  candidates and proposals all check membership first. `refuse_public` answering 409 only reveals
  public rooms, which are listed anyway.
- Round members resolved by email give one message whether or not the account exists
  (`rounds_api.rs:493`). Blind verdicts and votes are filtered by `may_read_verdicts`. Proposal
  documents are listed only when readable (`documents.rs`).
- Invitation and group-invitation previews (project or group name, inviter email) need the token.
  This is intended. Accepting an addressed invitation checks the email.
- Session cookie is `Secure; HttpOnly; SameSite=Lax` (`auth.rs:358`). The flash and OIDC cookies
  use the same flags. There is no CORS layer, and the cross-site write guard covers every path.
- Logs: there is no request tracing layer, so no URIs or query strings are logged. Mail failures
  log only the error, not the link. Relay errors and reports are masked. CLI output holds no
  secrets. Metrics labels are route templates only, and the endpoint has an optional constant-time
  bearer check.
- Admin dashboard and `/users/{id}`: superuser only (`admin.rs` `require`, `accounts_api.rs`
  `superuser`).
- `/openapi.json` and `/docs` are public, but schema only, with no data or examples.
- `known_people` suggestions only list people who share a membership or a group with the caller.
- Tokens API returns name and dates only. No IP addresses are stored anywhere.

## Unsure / not verified

- Whether lettre's `AsyncSmtpTransport` `Debug` would print SMTP credentials. `Mailer` derives
  `Debug`, but nothing formats it today.
- Whether an OIDC error's `Display` (`ui/oauth.rs:131`, logged at warn) can contain a provider
  response body.
- yrs GC on load (see 7).
