# Phase 3c — Sign in with ORCID (OpenID Connect) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let people sign in through any configured OpenID Connect provider (ORCID is one
entry), with Python's account rules, from the web UI's login page.

**Architecture:** An `oidc` module holds the provider list (from `COCAT_OAUTH_PROVIDERS`,
Python's JSON shape) and the account rules; `ui/oauth.rs` holds the two browser routes.
The protocol is done by the `openidconnect` crate: discovery, authorization URL with
**PKCE and a nonce** (Python had neither), code exchange, ID-token signature and claims
checks. Linked accounts live in a new `oauth_account` table that stores only
`(provider, subject) → user`: no provider tokens are kept, since cocat never calls the
provider after login.

**Tech Stack:** new: `openidconnect` 4.x (with its `reqwest` client, rustls, no default
TLS). Test-only: a fake provider served by axum inside the test, signing ID tokens with an
RSA test key committed under `tests/fixtures/` (generated once with `openssl genrsa`; the
README of the fixture says it's a test key).

**Spec:** design phase 3 ("ORCID via `openidconnect`"); phase 2b moved OAuth here with the
association rules of Python's `users.py::oauth_callback`. Behaviour reference: the Python
test checkout `src/cocat/app/oauth.py`, `src/cocat/ui/oauth.py`, `src/cocat/app/users.py`
(`oauth_callback`), tests `tests/test_oauth.py` and `tests/test_ui_oauth.py`.

## Global Constraints

- This repo, branch `main`, commit per task, never push.
  Rules file (established crates), TDD, one build/test command at a time in the
  foreground, full gate after the last edit of each task.
- Configuration, as Python: `COCAT_OAUTH_PROVIDERS` = JSON list of `{name:
  ^[a-z0-9_-]{1,32}$, discovery_url, client_id, client_secret, verifies_email: bool =
  false, scopes: [..] = ["openid", "email"]}`. Any provider requires `COCAT_PUBLIC_URL`
  (startup error otherwise). A provider whose discovery fails at startup is skipped with a
  warning; the server still starts.
- Redirect URI: `{COCAT_PUBLIC_URL origin}{prefix}/ui/oauth/{name}/callback`, from `Links`,
  never from the request.
- **Email trust (Python's rule):** the provider's address is used only when the operator set
  `verifies_email` *and* it is a valid address (`email_address` crate). Otherwise the
  account gets `{subject}@{name}.placeholder.cocat.org`. Take the email from the ID token
  claims, falling back to the userinfo endpoint when the token has none.
- **Association (Python's `oauth_callback`):**
  1. `(provider, subject)` already linked → that user.
  2. Else, association is on (`verifies_email` and a mailer is configured) and a
     *verified* local account has that email → link it. An *unverified* one →
     `?error=exists`.
  3. Else an account with that email exists → `?error=exists`.
  4. Else create the user (random password, `is_verified = verifies_email`), with the
     Personal room and verification exactly as `accounts::register` does.
  A placeholder address never associates and is never verified.
- After that: an inactive user → `/ui/login?error=credentials`. Otherwise set the session
  cookie and 303 to `/ui/confirm-email` for a placeholder address, else `/ui/rooms`.
- **Login route** `GET /ui/oauth/{name}/login`: rate-limited (the UI limiter; over the
  limit → 303 `/ui/login?error=too_many`); refused with 403 only when `Sec-Fetch-Site` is
  `cross-site` (Python also refused `none`, i.e. a typed URL or bookmark: a deliberate,
  tested difference). Stores the PKCE verifier, the nonce and the CSRF state in one
  short-lived cookie `cocat_oidc` (`Path={prefix}/ui/oauth/`, HttpOnly, Secure,
  SameSite=Lax, Max-Age 600), its value signed with the server secret (a JWT with its own
  audience, `cocat:oidc`), then 303 to the provider.
- **Callback** `GET /ui/oauth/{name}/callback?code&state`: a missing or bad cookie, or a
  state that doesn't match → 400 "Bad state". The cookie is cleared on every outcome. A
  missing `code`, a provider error, a failed exchange or a failed ID-token check → 303
  `/ui/login?error=provider`. Log the reason at `warn` without codes or tokens.
- Login page: one "Sign in with {name}" link per configured provider (3a left the slot).
  Account page: "This account signs in with {names}." and no password form when the user
  has a linked account (3a left the slot).
- `→ skipped:` Python's JSON API pair `/auth/{name}/authorize|callback`; nothing in
  SciQLop calls it. Add it if a client needs it.

## Review Focus

1. **Login CSRF / account swap:** a callback with a valid `code` but a state from another
   browser, or no `cocat_oidc` cookie. Expect 400, no session. → Task 2 test.
2. **ID token replay or forgery:** wrong signature, wrong `aud`, expired, wrong `nonce`.
   Expect `?error=provider`. → Task 2 tests.
3. **Unverified email takeover:** a provider *without* `verifies_email` returns an email
   equal to an existing local account. Expect a placeholder account (never linked). With
   `verifies_email` but the local account unverified: `?error=exists`. → Task 1 tests.
4. **Open redirect through the provider:** the redirect URI is always built from
   `COCAT_PUBLIC_URL`, never the Host header. → Task 2 test with a forged Host.
5. **A provider down at startup** doesn't stop the server and its link isn't shown.
   → Task 1 test.

---

### Task 1: Providers, the account table and the rules

**Files:** create `crates/cocat-server/src/oidc.rs`, `migrations/0005_oauth_accounts.sql`;
modify `lib.rs` (`AppState.oidc: Vec<Provider>`), `cli.rs` (load at startup), `db.rs`.
Test: `tests/oidc_rules.rs`.

**Interfaces:**
- Table `oauth_account(provider TEXT NOT NULL, subject TEXT NOT NULL, user_id TEXT NOT NULL
  REFERENCES "user"(id) ON DELETE CASCADE, created_at DATETIME NOT NULL DEFAULT
  CURRENT_TIMESTAMP, PRIMARY KEY (provider, subject))` + index on `user_id`.
- `pub struct ProviderConfig` (serde, with the defaults above; `client_secret` never in
  `Debug`), `pub fn providers_from_env(env) -> Result<Vec<ProviderConfig>, OidcError>`.
- `pub struct Provider { config, client: openidconnect core client }`,
  `pub async fn discover(configs, links) -> Vec<Provider>` (skips and warns on failure).
- `pub struct Identity { subject: String, email: Option<String> }`.
- `pub async fn sign_in(state, provider: &ProviderConfig, identity: Identity) ->
  Result<db::User, SignInError>` with `SignInError::{Exists, Db(..)}`: the association rules
  above, in one `BEGIN IMMEDIATE` transaction for the lookups and the inserts.
- `pub async fn linked_providers(pool, user) -> Vec<String>`.
- [ ] Tests (from `test_oauth.py`): `config_parses_python_shape`, `bad_name_refused`,
  `public_url_required`, `unreachable_provider_skipped` (review focus 5),
  `untrusted_email_becomes_placeholder`, `invalid_email_becomes_placeholder`,
  `linked_subject_signs_in`, `trusted_email_links_verified_account`,
  `trusted_email_unverified_account_exists`, `untrusted_email_never_links` (review focus 3),
  `trusted_without_mailer_does_not_link`, `new_user_gets_personal_room`,
  `placeholder_never_verified`, `deleting_the_user_drops_the_link`.
- [ ] Red, implement, gate. Commit "OpenID Connect providers and account rules".

### Task 2: The browser flow

**Files:** create `ui/oauth.rs`; modify `ui/mod.rs`, `templates/{login,account}.html`.
Test: `tests/ui_oauth.rs` with the fake provider (`tests/oidc_support/mod.rs`: discovery,
JWKS, authorize (not called by the server), token endpoint that checks the PKCE verifier
and returns an ID token signed with the test key, userinfo).

- [ ] Tests (from `test_ui_oauth.py`): `login_redirects_to_provider_with_pkce_and_nonce`,
  `provider_link_on_login_page`, `good_callback_signs_in`, `forged_state_400`,
  `missing_cookie_400` (review focus 1), `missing_code_provider_error`,
  `token_endpoint_error`, `bad_signature`, `wrong_audience`, `expired_token`, `wrong_nonce`
  (review focus 2), `no_email_goes_to_confirm_email`, `deactivated_account`,
  `taken_address_exists`, `typed_url_allowed_cross_site_refused`, `login_rate_limited`,
  `redirect_uri_ignores_host_header` (review focus 4), `account_page_names_the_provider`,
  `cookie_cleared_after_callback`.
- [ ] Red, implement, gate. Commit "Sign in with an OpenID Connect provider".

### Task 3: Docs

- [ ] README: the `COCAT_OAUTH_PROVIDERS` format with an ORCID example
  (`https://orcid.org/.well-known/openid-configuration`, scopes `["openid"]`, and why ORCID
  users land on confirm-email), the trust and association rules, and the differences from
  Python (PKCE, nonce, no stored tokens, bookmarks allowed, no JSON API pair). Spec status
  line: phase 3 done. Commit "Document OpenID Connect sign-in".
