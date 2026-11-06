# Phase 2b — Accounts — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serve the account routes the Python server gets from fastapi-users — register,
verify, forgot/reset password, logout, `/users/me`, the superuser `/users/{id}` routes —
plus outgoing mail, the mail-gated login, and emailed invitations.

**Architecture:** One `accounts` module for the rules (tokens, register, verify, reset,
profile update) and one `accounts_api` module for the routes, like 2a's `invitations` /
`invitations_api` split. Verification and reset tokens are short JWTs signed with the
server secret, each with its own audience. A `mail` module owns SMTP (lettre) and a
directory sink used by tests and local development. Links are always built from
`COCAT_PUBLIC_URL`, never from the request's `Host` header.

**Tech Stack:** existing crates; new: `lettre` 0.11 (`tokio1`, `tokio1-rustls-tls`,
`smtp-transport`, `builder`, `file-transport`; no default features), `sha2` (already in).

**Spec:** `docs/superpowers/specs/2026-11-05-cocat-rs-design.md` (phase 2, contract,
Risks: "Host-header reset links, placeholder addresses, OAuth association rules").
Behaviour reference: cocat-python-tests `rust-spike`: `src/cocat/app/{users.py,accounts.py,mail.py,
urls.py,app.py}` and fastapi-users 15 (`.venv/lib/python3.14/site-packages/fastapi_users/
{manager.py,router/*.py}`); tests `test_accounts.py`, `test_mail.py`, `test_login_gate.py`,
`test_ratelimit.py`.

**Moved out of 2b:** ORCID/OAuth. In Python it exists only as browser redirects mounted in
the web UI (`src/cocat/ui/oauth.py`, ending on UI pages); it moves to phase 3 with the UI,
keeping the association rules of `users.py::oauth_callback` (placeholder never associated,
unverified local account never taken over).

## Global Constraints

- Worktree `../cocat-rs-p1`, branch `phase-1`; never the
  `cocat-rs` main checkout. Rules file, TDD and the gate as before.
- Error bodies as fastapi-users: `{"detail": "<CODE>"}` for its error codes
  (`REGISTER_USER_ALREADY_EXISTS`, `LOGIN_BAD_CREDENTIALS`, `LOGIN_USER_NOT_VERIFIED`,
  `RESET_PASSWORD_BAD_TOKEN`, `VERIFY_USER_BAD_TOKEN`, `VERIFY_USER_ALREADY_VERIFIED`,
  `UPDATE_USER_EMAIL_ALREADY_EXISTS`); schema failures 422; unauthenticated 401; a
  non-superuser on `/users/{id}` 403; unknown id 404.
- Tokens, secrets, passwords and reset/verify links are never logged.
- Enumeration: `forgot-password` and `request-verify-token` answer 202 whatever the address;
  a mail failure is logged with the user id and never changes the reply.
- Rate limit (2a's limiter, `path|client` keys): `POST /auth/register`,
  `POST /auth/forgot-password`, `POST /auth/request-verify-token`, in addition to login.
- Passwords: no strength rule, like Python (its tests register with `"pwd"`). Flagged for
  the phase-4 security campaign, not decided here.

## Review Focus

1. **A reset link built from a spoofed `Host`.** Expected: a mailer cannot be configured
   without `COCAT_PUBLIC_URL` (startup error), and links use only that origin plus
   `COCAT_PROXY_PREFIX` (Task 1 test `links_ignore_the_host_header`).
2. **A reset token used twice, or after the password changed.** Expected: the token embeds a
   fingerprint of the current password hash, so the second use and any use after a change
   are `RESET_PASSWORD_BAD_TOKEN` (Task 3 test `a_reset_token_works_once`).
3. **A token of one kind used as another.** A verify token posted to reset, a session cookie
   posted as a reset token. Expected: rejected by audience (Task 3 test
   `tokens_are_not_interchangeable`).
4. **Changing one's email to dodge verification.** Expected: a changed email clears
   `is_verified` and, with a mailer, sends a new verification; a user cannot set
   `is_superuser`, `is_active` or `is_verified` on themselves (Task 4 test
   `a_user_cannot_raise_their_own_flags`).
5. **Mail header injection.** An address or room name with a newline. Expected: the message
   is refused before the relay, the request still answers as if mailed (Task 1 test
   `a_newline_in_a_header_is_refused`).

---

### Task 1: Outgoing mail and public links

**Files:**
- Create: `crates/cocat-server/src/mail.rs`, `crates/cocat-server/src/links.rs`
- Modify: `crates/cocat-server/src/lib.rs` (`AppState` gains `mailer: Option<Mailer>` and
  `links: Links`), `crates/cocat-server/src/cli.rs` (env), `Cargo.toml`, `README.md`
- Test: `crates/cocat-server/tests/mail.rs` (new)

**Interfaces:**
- Produces: `pub enum Mailer { Smtp(SmtpMailer), Directory(PathBuf) }`,
  `Mailer::from_env(env: &impl Fn(&str) -> Option<String>) -> Result<Option<Mailer>, MailError>`,
  `Mailer::send(&self, to: &str, subject: &str, body: &str) -> Result<(), MailError>`;
  `pub fn is_placeholder(email: &str) -> bool` (move it here from 2a, Python's rule).
  Environment, copied from `mail.py`: `COCAT_SMTP_HOST` (unset → no mailer),
  `COCAT_MAIL_FROM` (required with a host), `COCAT_SMTP_PORT` (587), `COCAT_SMTP_USERNAME` /
  `COCAT_SMTP_PASSWORD` (both or neither), `COCAT_SMTP_STARTTLS` (`1/true/yes/on`,
  `0/false/no/off`, default on), 10 s timeout. `COCAT_MAIL_DIR` (new): write each message as
  an `.eml` file there instead of sending (lettre's file transport) — the test sink and a
  local development aid; mutually exclusive with `COCAT_SMTP_HOST`.
- Produces: `pub struct Links { origin: String, prefix: String }`,
  `Links::from_env(...) -> Result<Links, ...>`, `Links::ui(&self, path_and_fragment: &str) -> String`
  → `{origin}{prefix}/ui{path}`. `COCAT_PUBLIC_URL` keeps only scheme and host
  (Python `urls.py::public_base`); `COCAT_PROXY_PREFIX` normalised like `prefix.py`
  (`"/" + stripped` or empty). A configured mailer without `COCAT_PUBLIC_URL` is a startup
  error naming the variable (Python `test_a_mailer_needs_a_public_url`).
- Mail texts verbatim from `mail.py`: `verification_mail`, `reset_mail`, `invitation_mail`.

- [ ] **Step 1: Failing tests:** `no_host_means_no_mailer`, `a_host_needs_a_sender`,
  `username_needs_password`, `starttls_words` (each accepted word, and a bad one names the
  variable), `a_placeholder_receives_no_mail`, `a_newline_in_a_header_is_refused`,
  `the_directory_mailer_writes_one_file_per_message` (read the `.eml`, check To/Subject/body),
  `links_ignore_the_host_header`, `links_carry_the_proxy_prefix`,
  `a_mailer_needs_a_public_url`.
- [ ] **Step 2-5:** run red, implement, full gate, commit "Add outgoing mail and public links".

### Task 2: Register, logout, the mail-gated login

**Files:**
- Create: `crates/cocat-server/src/accounts.rs`, `crates/cocat-server/src/accounts_api.rs`
- Modify: `crates/cocat-server/src/auth.rs` (login gate), `crates/cocat-server/src/db.rs`
  (`create_user` takes `is_verified`), `crates/cocat-server/src/ws.rs` (routes)
- Test: `crates/cocat-server/tests/accounts.rs` (new)

**Interfaces:**
- `POST /auth/register {"email","password"}` → 201 UserRead (2a's shape). Duplicate (any
  case) → 400 `REGISTER_USER_ALREADY_EXISTS`; invalid or placeholder address → 422. Creates
  the personal room (2a's `db::create_personal_room`). Verification, from
  `accounts.py::start_verification`: no mailer → the account is verified at once; a mailer →
  `is_verified = false` and a verification mail (Task 3's token) is sent best-effort.
- Login gate: with a mailer, an unverified account's login is 400 `LOGIN_USER_NOT_VERIFIED`
  (after the password check, so it does not reveal unverified addresses to a wrong
  password). Without a mailer, unchanged. Bad credentials stay `LOGIN_BAD_CREDENTIALS`
  (check what the spike returns today and align).
- `POST /auth/jwt/logout` (authenticated, else 401) → 204 with the cookie cleared
  (`Max-Age=0`, same name, path and attributes).
- The CLI `create-user` keeps creating verified accounts.

- [ ] **Step 1: Failing tests:** `register_creates_a_verified_account_without_a_mailer`,
  `register_with_a_mailer_sends_a_verification_and_leaves_it_unverified`,
  `register_creates_the_personal_room`, `a_duplicate_email_any_case_is_refused`,
  `a_placeholder_address_cannot_register` (422),
  `without_a_mailer_an_unverified_account_logs_in`,
  `with_a_mailer_an_unverified_account_is_refused`,
  `a_wrong_password_on_an_unverified_account_is_bad_credentials`,
  `logout_clears_the_cookie`, `register_is_rate_limited`.
- [ ] **Step 2-5:** red, implement, gate, commit "Register, log out, gate login on verification".

### Task 3: Verification and password reset

**Files:**
- Modify: `crates/cocat-server/src/accounts.rs`, `crates/cocat-server/src/accounts_api.rs`,
  `crates/cocat-server/src/auth.rs` (shared JWT helpers)
- Test: `crates/cocat-server/tests/accounts.rs`

**Interfaces:**
- Tokens: HS256 with the server secret, 3600 s. Verify token claims `{sub, email, aud:
  "cocat:verify"}`; reset token claims `{sub, password_fgpt, aud: "cocat:reset"}` where
  `password_fgpt` = SHA-256 hex of the current `hashed_password` (fastapi-users hashes it
  with the password helper; any one-way function of the hash serves the same purpose). The
  session audience stays as it is.
- `POST /auth/request-verify-token {"email"}` → 202 always; mails a verify link
  (`Links::ui("/verify#{token}")`) when the account exists, is active, unverified and not a
  placeholder.
- `POST /auth/verify {"token"}` → 200 UserRead with `is_verified: true`; bad, expired, or
  for an email that no longer matches the account → 400 `VERIFY_USER_BAD_TOKEN`; already
  verified → 400 `VERIFY_USER_ALREADY_VERIFIED`.
- `POST /auth/forgot-password {"email"}` → 202 always; mails a reset link
  (`Links::ui("/reset-password#{token}")`) when the account exists and is active.
- `POST /auth/reset-password {"token","password"}` → 200; bad, expired, fingerprint
  mismatch, or inactive user → 400 `RESET_PASSWORD_BAD_TOKEN`.

- [ ] **Step 1: Failing tests:** `verify_flow` (register with a mailer, read the link from the
  directory mailer, verify, login works), `verify_twice_is_already_verified`,
  `a_verify_token_for_an_old_email_is_bad`, `forgot_password_answers_202_for_anyone`,
  `reset_flow`, `a_reset_token_works_once`, `an_expired_token_is_bad` (mint with a past
  `now`, no sleep), `tokens_are_not_interchangeable` (verify↔reset, session cookie as either),
  `no_token_or_link_reaches_the_log` (capture tracing output during the flows, assert the
  token string is absent), `forgot_password_is_rate_limited`.
- [ ] **Step 2-5:** red, implement, gate, commit "Verify addresses and reset passwords".

### Task 4: `/users/me` and the superuser routes; emailed invitations

**Files:**
- Modify: `crates/cocat-server/src/accounts.rs`, `crates/cocat-server/src/accounts_api.rs`,
  `crates/cocat-server/src/invitations.rs` (send when a mailer exists),
  `crates/cocat-server/src/db.rs`
- Test: `crates/cocat-server/tests/accounts.rs`, `crates/cocat-server/tests/invitations.rs`

**Interfaces:**
- `GET /users/me` → UserRead. `PATCH /users/me {"email"?, "password"?}` → UserRead; any other
  field (`is_superuser`, `is_active`, `is_verified`) is ignored for a non-superuser
  (fastapi-users `safe=True`). A new email that exists → 400
  `UPDATE_USER_EMAIL_ALREADY_EXISTS`; a changed email sets `is_verified = false` and, with a
  mailer, sends a verification.
- `GET/PATCH/DELETE /users/{id}`: superuser only (403 otherwise, 404 unknown); PATCH may set
  every flag; DELETE → 204 and revokes the user's sockets in every room
  (`RoomManager::revoke` per membership, before the row goes).
- Invitations with an email and a mailer (Python `actions.py::invite`): the token is mailed
  (`invitation_mail(room_name, inviter_email, Links::ui("/invitation#{token}"))`) and the
  API returns `{"id", "token": ""}`; a send failure deletes the invitation and answers 400
  `"The email could not be sent; no invitation was created"`.

- [ ] **Step 1: Failing tests:** `users_me_reads_and_updates`,
  `a_user_cannot_raise_their_own_flags`, `changing_email_clears_verification`,
  `an_existing_email_is_refused`, `only_a_superuser_reaches_users_by_id`,
  `deleting_a_user_closes_their_sockets` (live socket, 1008),
  `an_emailed_invitation_returns_no_token_and_mails_the_link`,
  `a_failed_invitation_mail_creates_nothing` (point the directory mailer at an unwritable
  path).
- [ ] **Step 2-5:** red, implement, gate, commit "Add the profile and superuser routes and mail invitations".

### Task 5: Python live slice

**Files (cocat-python-tests `rust-spike`):** `tests/conftest.py` (`LIVE_SKIP`), and the live branch of
the mail fixtures (`app_client_mail`) if it can point the Rust server at `COCAT_MAIL_DIR`.

- [ ] **Step 1:** Release build; run the live slice over `tests/test_routes.py`,
  `tests/test_login_gate.py`, `tests/test_ratelimit.py` and any other file whose tests only use
  HTTP. Remove the 2b entries from `LIVE_SKIP`; make the mail fixtures read the Rust server's
  `.eml` files in live mode if that is a small shim, else skip with the reason and report.
- [ ] **Step 2:** Normal suite unchanged (coverage 100 %, mypy, ruff).
- [ ] **Step 3:** One commit per repo. Do not push.
