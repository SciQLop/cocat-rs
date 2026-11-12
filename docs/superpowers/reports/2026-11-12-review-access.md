# cocat-rs — access control and authentication review

Scope: `git diff fd05753 HEAD -- crates` (HEAD = bc10cad), focus on who may call what.
Method: read-only. Every route in `api.rs`, `accounts_api.rs`, `auth.rs`, `sign_in_api.rs`,
`ui/mod.rs`, `ws.rs` was traced to its guard. Nothing was built or run. Findings that I
could not prove by running code say so.

All paths below are relative to `crates/cocat-server/src/` unless stated.

## Findings, by severity

### M1 — `PATCH /users/me` changes password and email with no current password (medium)

`accounts_api.rs:224` (`update_me`) passes `password` and `email` straight to
`accounts::change`. The web page asks for the current password (`ui/account.rs:112`), but
the API does not. Any bearer of a session or an app token can therefore take the account.

Scenario (device-style phishing, the new `/ui/connect` flow makes this cheap):
1. Attacker calls `POST /api/sign-in-requests` with name "Python on lpp-jupyter" and mails the
   victim the returned `/ui/connect?code=…` link.
2. Victim, signed in, clicks Allow. The attacker's poll gets a 6-month app token.
3. Attacker sends `PATCH /users/me {"email": "attacker@x"}` with `Authorization: Bearer`.
   The address becomes unverified and a verification mail goes to the attacker.
4. Attacker verifies, then `POST /auth/forgot-password` for attacker@x and resets the
   password. The reset ends every session, the victim's included.
5. The attacker now owns the account outright; the victim cannot sign in or reset.

Even without step 4, `PATCH /users/me {"password": …}` locks the victim's browsers out
(`end_browser_sessions_of`) while keeping the attacker's token alive (tokens survive a
password change by design, `accounts.rs:216-220`).
Fix: require `current_password` on the API too (same as the UI), at least for email and
password changes. Consider refusing these two fields to named app tokens entirely.

### M2 — Logout and password change do not close live websockets (medium)

Sockets close on a session's end only when `RoomManager::end_session` is broadcast
(`ws.rs` `this_session_ended`). Three paths delete sessions without broadcasting:
- `auth::end_session` (`auth.rs:198-204`), used by `POST /auth/jwt/logout`
  (`auth.rs:442`) and `POST /ui/logout` (`ui/login.rs:111`).
- Password change: `accounts.rs:219` calls `db::end_browser_sessions_of`, which returns
  nothing and broadcasts nothing (`db.rs:867`).
- Session expiry: the 30 s `check_roles` loop (`ws.rs:478-505`) re-reads the role only,
  never the session, so an expired session's socket lives on.

Scenario: someone steals a cookie and opens `/room/{id}`. The victim notices, logs out
everywhere they can, and changes their password. The thief's socket keeps reading every
update and, as editor, keeps writing, until the TCP connection drops or their role changes.
The phase-4b fix 1-01 made the cookie invalid for new requests; it did not reach open sockets.
By contrast `DELETE /api/tokens/current` (`tokens_api.rs:81-82`), token revoke and reset
do broadcast, so the gap is an inconsistency, not a design choice.
Fix: make `auth::end_session` take the state and call `state.rooms.end_session(&hash)`;
make `end_browser_sessions_of` return hashes (`RETURNING token_hash`) and broadcast them;
optionally add `session_alive` to the periodic recheck.

### M3 — Websocket handshake has no Origin check: same-site socket hijacking (low; medium if sibling subdomains host other content)

`ws.rs:108` `open_socket` authenticates from the cookie and upgrades. The cross-site
guard `ui::same_site` (`ui/frame.rs:379`) skips GET, and a websocket handshake is a GET.
`SameSite=Lax` keeps the cookie off a cross-*site* handshake, but a page on any sibling
subdomain of the same registrable domain (e.g. another `*.sciqlop.fr` app, or an XSS
there) is same-site: the browser sends the cookie, and `new WebSocket(url, [SUBPROTOCOL])`
satisfies the subprotocol check. WebSockets have no CORS, so that page gets full read and
(as editor) write access to every project of the visitor. Every other state-changing route
is protected by the Origin check, so this is the one hole.
Fix: in `open_socket`, refuse when `Origin` is present and its authority differs from
`Host` (reuse `cross_site`), or when `Sec-Fetch-Site` is not `same-origin`.

### L1 — Open redirect through `next` with dot segments (low; likely, not executed)

`ui/login.rs:150-182` `safe_next` resolves `next` against a placeholder origin and keeps
`joined.path()`. A value like `/.//evil.com` or `/x/..//evil.com` keeps the placeholder
host (so the "local" test passes) but WHATWG/rust-url path normalization turns it into the
path `//evil.com`. With no `COCAT_PROXY_PREFIX`, `links.prefixed` returns it unchanged
(line 181) and it lands in `Location: //evil.com`, a protocol-relative URL to another host.
The tests (`tests/ui_login.rs:387-404`) cover `//evil.com` and tab tricks, not dot
segments. New since fd05753: `GET /ui/login?next=…` by an already signed-in visitor now
redirects immediately (`ui/login.rs:50`), and OIDC carries `next` (`ui/oauth.rs:201`).
I did not run this; it follows from the URL spec, and a one-line test would settle it.
Fix: after building `target`, refuse anything starting with `//` (or `/\`).

### L2 — No anti-framing header anywhere (low)

`grep` finds no `X-Frame-Options` or `frame-ancestors`. The `/ui/connect` Allow button,
`/ui/account` and the admin forms can be framed. Cross-site framing gets no cookie
(`SameSite=Lax`), so the frame is signed out; a same-site sibling page can frame it signed
in and clickjack "Allow" on an attacker's sign-in code (which hands it a token, see M1).
Fix: `X-Frame-Options: DENY` (or CSP `frame-ancestors 'none'`) on `/ui` responses.

### L3 — Blobs served without `nosniff` or `attachment` (low)

`blobs.rs:224` answers `Content-Type: application/octet-stream` only. Current browsers do
not render octet-stream as HTML, so this is defence in depth: add
`X-Content-Type-Options: nosniff` and `Content-Disposition: attachment`. Upload handling
itself is fine: the name is the verified SHA-256 (`valid_hash`, `blobs.rs:65`), files go
under `blobs/<escaped room id>/<hash>`, and the body is capped (`read_body`).

### L4 — Sign-in codes: device-flow phishing is unmitigated (low, inherent)

The connect page (`ui/connect.rs`) shows only the attacker-chosen name. Nothing ties the
request to the person who started it. This is how device flow works; the risk is what M1
makes of the resulting token. Mitigations: show when and from which IP the request was
made, and warn "only allow an app you just started yourself". The 40-bit code is not
brute-forceable in its 10-minute life; `POST /ui/connect` is unthrottled but that is fine
at that size.

### L5 — App tokens carry the whole account (low / design)

`auth::authenticate` treats a named token exactly like a browser session: it reaches the
`/ui` pages, `/users/me`, project deletion, sharing, and a superuser's admin powers. There is
no scope. Combined with M1 this is takeover. Suggest at least: tokens cannot change email
or password, cannot approve sign-in requests, and do not carry superuser rights.

### L6 — Relay redirect policy allows a scheme or port change on the same host (low)

`relay/mod.rs:155` `follows` compares only `host_str`. A provider (or a MITM on a plain-HTTP
hop) can redirect `https://host/` to `http://host/` or `https://host:8443/`. No SSRF beyond
the provider's own host: every provider URL is a compile-time constant, no provider declares
settings (all `settings()` are empty), AMDA links must start with `https://amda.irap.omp.eu/`,
and catcsv links must be `https` on the site. Fix: also require `next.scheme() == "https"`
and the same port. Related, not access control: `relay/patient.rs` trusts `Retry-After`
without a cap (a provider can park a run for days with `running` set) and reads bodies
without a size cap.

### L7 — Invitation tokens now travel in a cookie (info)

`ui/frame.rs:184` `share_link` puts the new invitation URL (token included) in the
`cocat_share_link` cookie for one minute. It is `HttpOnly; Secure; SameSite=Lax`, path
`/ui`. The phase-4b report listed "never in flash cookies" as a property; this changes it.
Risk is only proxies that log cookies.

### L8 — Metrics method label is unbounded (info, DoS)

`telemetry.rs:157` labels requests by raw HTTP method. Arbitrary extension methods
(`FOO1`, `FOO2`, …) create a new Prometheus series each, growing memory without bound.
Map unknown methods to `"other"`. The `/metrics` listener itself is separate and optionally
token-protected (`COCAT_METRICS_TOKEN`), constant-time compared.

## Checked and found fine (false positives)

- **Document / proposal IDOR on sockets and history.** `/room/{project}/{doc}` and
  `?document=` resolve through `access::content`, whose queries filter by project
  (`db::document_kind` `WHERE room_id = ? AND id = ?`, `rounds::proposal_by_doc` joins
  `round.room_id`). A doc id from another project is 404.
- **Blind rounds.** Proposal docs, verdicts and votes are filtered through
  `rules::proposal_role` / `may_read_verdicts`; `documents::list_documents` lists only
  readable proposals; history `list`/`state_at`/`restore` go through `history_key`;
  `changes` refuses documents. A demoted decider loses rights (`is_decider` needs
  `can_write`). Round mutations call `check_room_roles`.
- **Round ids.** `rounds_api::load` checks `row.room_id == room_id` and membership.
  Members named in a round must be project members; deciders must be editors.
  Candidate copies require membership of the source project (`round_candidates.rs:31`).
- **History.** Reads need membership; `name`/`unname`/`commit`/`restore` need Editor
  (and refuse relayed). `copy` copies only what a member can already read. Public
  catalogue history hides author emails and ids from non-curators.
- **Submissions.** `submit`/`submit_update` need Editor on the source project;
  `one`/`withdraw` only author or curator; `review` only curator; the target of an update
  comes from a link `propose` made to an existing public catalogue, not from the body.
  `copy_into` needs Editor on the destination and refuses public ones. Adopting keeps the
  public catalogue's own copy chain (`restore_map` skips `copied_from`), so a proposer's
  copy chain does not leak into the public catalogue.
- **Public catalogues.** Create/patch need curator; relayed ones are read-only for
  everyone, curators and superusers included (`role_in` CASE, `require_role` →
  `refuse_relayed`). Members, invitations, groups and transfers refuse public rooms.
- **Blobs.** Viewer to read, Editor to write, per-project lookup; `%2F` in a room id only
  reaches `role_in`, which finds no membership.
- **Groups.** Rename/delete/invite need group owner; grant needs room sharer and group
  membership; owner role cannot be granted; delete, remove, ungrant and accept revoke the
  affected sockets after commit. Addressed invitations compare the email.
- **Room invitations.** `require_sharer` (owner, or editor when allowed); owner role
  refused; personal rooms refused; `accept_addressed`/`decline` need a verified email.
- **Admin.** Every `admin.rs` and `relay/admin.rs` function calls `require(actor)`
  (superuser) first, including the UI routes that skip the early check (`delete_user`,
  `set_owner`, `delete_room`, providers). `/users/{id}` routes check superuser.
- **Tokens API.** List/revoke are scoped to the caller (`DELETE … WHERE user_id = ? AND
  id = ?`), revoke broadcasts the session end.
- **Sign-in polling.** The approved request is deleted and turned into a token in one
  statement; polls are slowed per request; creation is login-rate-limited.
- **CSRF.** `ui::same_site` wraps the whole router (`ws.rs:76`), so every non-GET on `/api`,
  `/auth`, `/users` and `/ui` is Origin/Sec-Fetch-Site checked. No GET route changes state
  (`/ui/connect` GET only displays). The websocket is the exception (M3).
- **Stored XSS in the web app.** Askama escapes the app head (`templates/app_head.html`).
  Markdown is sanitized (raw HTML to text, link schemes whitelisted, control characters
  stripped). Notebook HTML outputs go in `<iframe sandbox="">` with a CSP meta; SVG
  outputs are `<img>` data URLs; stored SVG blobs are downloads, not images.
- **Links between projects.** They are pointers in the visitor's own document; the app
  reads the target with the visitor's own credentials, so the server enforces membership.
  No server-side confused deputy.
- **Revocation of roles on sockets.** Member removal, role change, transfer, group
  changes, deactivation, deletion, curator/superuser flag and reset all close sockets;
  the 30 s recheck covers the CLI. Only session ends are missed (M2).

## Not covered / leads

- Markdown images may point at any `http(s)` URL: a notebook author learns the IP of each
  viewer (tracking pixel). Privacy, low.
- `GET /api/public` counts every changed public catalogue on each call (any account can
  trigger room loads). Resource use, not access.
- OIDC account linking itself was unchanged since fd05753 and not re-reviewed.
