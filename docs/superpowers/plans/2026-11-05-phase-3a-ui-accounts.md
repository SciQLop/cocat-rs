# Phase 3a — Web UI: frame and account pages — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serve the account side of the web UI under `/ui` as server-rendered HTML: login,
logout, sign up, resend verification, forgot/reset password, verify, account and
confirm-email, with the same paths, messages and redirects as the Python UI.

**Architecture:** `askama` templates compiled into the binary, one `ui` module tree
(`ui/mod.rs` router + helpers, one file per page group). Every action is a plain HTML form
POST answered with a 303 redirect (post/redirect/get); messages travel as query flags
(`?error=credentials`, `?created=1`), exactly as in Python. No client framework: the only
script is a ~15-line static `fragment.js` that moves a `#token` fragment into a hidden form
field and clears it with `history.replaceState`. htmx is *not* needed for these pages;
phase 3b decides whether its room page wants it.

**Tech Stack:** new: `askama` (current release; derive templates, auto-escaping on). Static
assets (`style.css`, `fragment.js`) embedded with `include_str!` and served with long
cache headers. No CDN.

**Spec:** `docs/superpowers/specs/2026-11-05-cocat-rs-design.md` phase 3. Behaviour
reference: the Python test checkout `/home/jeandet/Documents/prog/cocat-python-tests`,
`src/cocat/ui/{__init__,auth,login,signup,reset,verify,account,confirm_email,layout,token}.py`
and their tests `tests/test_ui_{mount,login,signup,reset,verify,account,confirm_email}.py`,
`tests/test_prefix.py`. Read the Python page *and* its test before each task: the test
names below come from them.

## Global Constraints

- Repo `/home/jeandet/Documents/prog/cocat-rs`, branch `main`, commit per task, never push.
  Rules file, TDD, one build/test command at a time in the foreground, full gate after the
  last edit of each task:
  `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --workspace`.
- Paths are Python's: pages under `{COCAT_PROXY_PREFIX}/ui/...`; every link and redirect is
  an absolute path that includes the prefix (`Links` already knows the prefix; add a
  `Links::path(p)` that returns `{prefix}/ui{p}` without the origin).
- Session: the existing `fastapiusersauth` cookie and JWT from `auth.rs`; no new session
  mechanism. Anonymous on a page that needs a user → 307 to
  `/ui/login?next=<url-encoded request path>` (path only, no query).
- **Cross-site POSTs are refused with 403** using Python's `same_site` rule (Origin host must
  equal Host; `Sec-Fetch-Site`, when present, must be `same-origin` or `same-site`; absent
  headers pass). One axum middleware or extractor applied to every UI POST. No CSRF tokens:
  `SameSite=Lax` plus this check is the model, as in Python. Phase 4 attacks it.
- Rate limits (existing limiter, 10 per 60 s per path and client): POST `/ui/login`,
  `/ui/signup`, `/ui/forgot-password`, `/ui/request-verify`. Over the limit → 303 back to the
  form with `?error=too_many`, not a 429.
- `?next=` is honoured only by POST `/ui/login`, through `safe_next` (refuse a scheme, a
  host, a backslash, a leading `//`, a path not starting with `/`; fallback `/ui/rooms`).
- Messages are Python's, verbatim (the table in `ui/auth.py` `MESSAGES` and the page texts).
  Templates auto-escape; nothing user-supplied is ever marked safe.
- Mailed links keep their current shape (`/ui/verify#token`, `/ui/reset-password#token`).
- Deliberate differences from Python, each with a test:
  1. Signup with a taken address answers the same as a fresh one when mail is configured
     (`?created=1`, and the existing owner gets a "someone tried to sign up" mail instead).
     Without mail there's no verification step, so Python's `?error=exists` stays.
  2. Sending mail never happens inline in a form POST's response path for forgot-password
     and request-verify: it's spawned, so the answer time doesn't reveal whether the
     address exists.

## Review Focus

1. **`next` as an open redirect:** `//evil.com`, `/\evil.com`, `https://evil.com`,
   `/%2F%2Fevil.com`, `javascript:alert(1)`. Expect: all fall back to `/ui/rooms`.
   → Task 2 tests.
2. **A user value in a page:** an email or room name containing `<script>` or `"><` shows
   as text. → Task 1 test with a crafted email on the account page.
3. **Cross-site form POST** with a foreign `Origin`, or with `Sec-Fetch-Site: cross-site`,
   on every UI POST route. Expect 403 and no side effect. → Task 1 test, table-driven over
   all routes added in 3a.
4. **A token in a fragment** never reaches server logs: the reset and verify forms post it
   in the body, never in a query string. → Task 3 test asserts the form method and field.
5. **Deactivated user with a still-valid cookie** opens a page or posts a form. Expect:
   treated as anonymous (redirect to login), no action taken. → Task 4 test.

---

### Task 1: The frame

**Files:** create `crates/cocat-server/src/ui/{mod.rs,frame.rs}`,
`crates/cocat-server/templates/{base.html,message.html}`,
`crates/cocat-server/static/{style.css,fragment.js}`; modify `Cargo.toml`, `ws.rs` (mount
`ui::router(&state)` under `/ui`), `links.rs`. Test: `crates/cocat-server/tests/ui_frame.rs`.

**Interfaces (produced):**
- `pub fn router(state: &AppState) -> Router<AppState>` in `ui/mod.rs`.
- `pub(crate) struct Page<T>`-style or plain askama structs; a shared `Header { email,
  rooms_href, account_href, logout_action }` rendered by `base.html` when a user is present.
- `pub(crate) async fn same_site(req, next)` middleware (refuses with 403, plain text body
  "Forbidden").
- `pub(crate) fn message(query) -> &'static str`: Python's `error_key` rule over the
  `MESSAGES` table (first truthy flag among `created`, `sent`, `reset`, `resent`, else
  `error`; unknown key → empty).
- `pub(crate) struct MaybeUser(Option<User>)` and `RequireUser(User)` extractors;
  `RequireUser` rejects with the 307 to login described above.
- `GET /ui/` → 307 `/ui/rooms`. `GET /ui/static/{file}` for the two assets.
- A placeholder `GET /ui/rooms` that renders the header and "No rooms page yet" so the
  redirects of this phase land somewhere; 3b replaces it.

- [ ] Tests first: `root_redirects_to_rooms`, `rooms_requires_login_with_next`,
  `static_assets_served_with_cache_headers`, `prefix_applies_to_links_and_redirects` (with
  `COCAT_PROXY_PREFIX=/cocat`), `user_text_is_escaped` (review focus 2),
  `cross_site_posts_are_refused` (review focus 3; a table of routes that grows with each
  task: add each new POST route to it), `message_table_matches_python`.
- [ ] Red, implement, gate. Commit "Serve the UI frame under /ui".

### Task 2: Login, logout, resend verification

**Files:** `ui/login.rs`, `templates/login.html`. Test: `tests/ui_login.rs`.

- `GET /ui/login`: signed in → 307 `/ui/rooms`; else the Python page (login form posting
  `username`/`password` to `/ui/login` with `?next` re-encoded, message, "Sign up" and
  "Forgot your password?" links, the resend form, one "Sign in with {name}" link per OAuth
  provider: none until 3c, so the list is empty but the template has the slot).
- `POST /ui/login`: same_site → rate limit → authenticate (reuse `auth.rs`'s credential
  check, which already equalises timing) → unverified-with-mail gate → set cookie, 303 to
  `safe_next(next)`. Errors as in Python (`?error=credentials|too_many|unverified`, `next`
  dropped).
- `POST /ui/logout`: same_site, clear cookie, 303 `/ui/login`. Not rate-limited.
- `POST /ui/request-verify`: same_site → rate limit → spawn `accounts::request_verification`
  → 303 `/ui/login?resent=1` always.
- [ ] Tests (from `test_ui_login.py`): `form_posts_the_password`, `login_sets_cookie_and_redirects`,
  `default_next_is_rooms`, `bad_password`, `inactive_user_refused`, `unverified_with_mail_refused`,
  `local_next_kept_with_query`, `foreign_next_refused` (review focus 1, table of the five
  inputs), `login_rate_limited`, `logout_clears_cookie`, `logout_not_rate_limited`,
  `resend_answers_the_same_for_unknown_and_known`, `resend_mails_the_unverified`,
  `login_flood_does_not_block_signup`.
- [ ] Red, implement, gate. Commit "Log in and out from the web UI".

### Task 3: Sign up, forgot and reset, verify

**Files:** `ui/signup.rs`, `ui/reset.rs`, `ui/verify.rs` and their templates. Test:
`tests/ui_signup.rs`, `tests/ui_reset.rs`, `tests/ui_verify.rs`.

- `GET/POST /ui/signup`: Python's rules (address validation including the placeholder
  domain → `?error=email`; rate limit; `accounts::register` which already creates the
  Personal room and starts verification; with mail → `/ui/login?created=1`; without mail →
  logged in, 303 `/ui/rooms`). Taken address: deliberate difference 1.
- `GET/POST /ui/forgot-password`: always `?sent=1`; the mail is spawned (difference 2).
- `GET /ui/reset-password`: the form has hidden `token`, filled by `fragment.js`, plus a
  visible fallback text box "Reset link" for pasting when scripts are off. `POST
  /ui/reset-password` (same_site; not rate-limited, as in Python): Python's checks in order
  ("Enter a new password", "The new passwords differ", "This reset link is not valid
  anymore."), re-rendering the form with the message (status 200 for validation, 400 for a
  bad token); success → 303 `/ui/login?reset=1`. The token is never echoed back in the page.
- `GET /ui/verify` + `POST /ui/verify` (the fragment is posted by `fragment.js` on load;
  without scripts, a "Verify" button): Python's four messages and the "My rooms" link.
- [ ] Tests (from `test_ui_signup.py`, `test_ui_reset.py`, `test_ui_verify.py`), including
  `reset_token_travels_in_the_body` (review focus 4: the form is `method="post"` and the
  token field is not in any `action` URL), `taken_address_with_mail_looks_like_success`,
  `taken_address_without_mail_says_exists`, `forgot_mail_is_not_awaited` (a mailer that
  sleeps 2 s; the POST answers in under 500 ms).
- [ ] Red, implement, gate. Commit "Sign up, reset and verify from the web UI".

### Task 4: Account and confirm-email

**Files:** `ui/account.rs`, templates. Test: `tests/ui_account.rs`.

- `GET /ui/account` (RequireUser): email; "Confirm your email" link for a placeholder
  address; password form (no OAuth accounts exist until 3c, so the "signs in with" branch is
  a template slot fed by an empty list). `POST /ui/account/password`: Python's messages in
  order; success re-renders with "Password changed".
- `GET/POST /ui/confirm-email` (RequireUser; non-placeholder → 307 `/ui/account`):
  Python's messages; reuse `accounts::change` for the address and its verification.
- [ ] Tests (from `test_ui_account.py`, `test_ui_confirm_email.py`) plus
  `deactivated_cookie_is_anonymous` (review focus 5: page → login redirect; POST → no
  password change).
- [ ] Red, implement, gate. Commit "Account pages in the web UI".

### Task 5: Docs

- [ ] README "Web UI" section: the pages, the same-site rule, the two deliberate
  differences from Python. Commit "Document the web UI account pages".
