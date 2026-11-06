# Phase 4d — OpenAPI document and Swagger UI — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serve an OpenAPI 3.1 document of the JSON API at `/openapi.json` and Swagger UI at
`/docs`, both public, as the Python (FastAPI) server does, generated from the code so a JSON
route cannot exist without being documented.

**Architecture:** `utoipa` annotations on every JSON handler and `ToSchema` on every request
and response type; the JSON routers are built with `utoipa_axum::router::OpenApiRouter` and
`routes!(..)`, which registers a route in the router and in the document at the same time;
`split_for_parts()` gives the axum `Router` and the `OpenApi` value. Swagger UI is
`utoipa-swagger-ui` with the `vendored` feature (bundled in the binary: no download at build
time, no CDN at runtime).

**Tech Stack:** new: `utoipa = "6"`, `utoipa-axum = "0.3"`, `utoipa-swagger-ui = { version =
"10", features = ["axum", "vendored"] }`. Checked: they resolve with axum 0.8 and our
`time 0.3.55`. Dev: `schemathesis` (Python, run once by hand, not a cargo dependency).

**Spec:** design phase 4 ("schemathesis over the OpenAPI document"); user decision
2026-11-06: public, as Python.

## Global Constraints

- Repo `/home/jeandet/Documents/prog/cocat-rs`, branch `main`, commit per task, never push.
  Rules file, TDD, ONE cargo command at a time in the foreground, full gate after the last
  edit of each task.
- **In the document:** every route under `/api`, `/auth` (including `/auth/jwt/login` and
  `/auth/jwt/logout`), `/users`, and the legacy `GET /rooms` and `GET /room/{room_id}/users`.
  **Not in it:** `/ui/*` pages, the websocket `GET /room/{room_id}`, `/docs`, `/openapi.json`.
- No behaviour change to any existing route: paths, status codes, bodies and error details
  stay byte-identical (the Python-parity tests must pass unchanged).
- `info.title` "cocat", `info.version` the crate version (`env!("CARGO_PKG_VERSION")`).
  Security scheme: an `apiKey` in cookie named `fastapiusersauth`, applied to every route
  that needs a session (not to register, login, forgot/reset/verify).
- Each handler's annotation lists its real responses (2xx, 400/401/403/404/409/422/429 as
  the code returns them) with the body type; `{"detail": ...}` errors share one schema.
- `COCAT_PROXY_PREFIX`: the document's `servers` is `[{url: <prefix or "/">}]` so Swagger UI
  calls the right paths behind the proxy; Swagger UI fetches `{prefix}/openapi.json`.
- `/docs` and `/openapi.json` are public GETs: add them to the access matrix as allowed for
  every caller.

## Review Focus

1. **A JSON route missing from the document** (the test below must fail when one is added
   with plain `.route()`).
2. **A response the code returns but the document doesn't list** — schemathesis finds these.
3. **Swagger UI broken behind `COCAT_PROXY_PREFIX`.**
4. **A build that downloads anything** (check the build log offline: `cargo build --offline`
   after a first fetch).

---

### Task 1: Document the API and serve it

**Files:** every `*_api.rs`, `api.rs`, `auth.rs` (login/logout), `ws.rs` (router assembly);
create `src/openapi.rs` (the `#[derive(OpenApi)]` root: info, security scheme, servers, the
`/docs` + `/openapi.json` routes). Test: `tests/openapi.rs`.
- [ ] Tests first: `openapi_json_is_served` (200, parses as JSON, `openapi` starts with
  "3.1"), `docs_page_is_served` (200, HTML that references `openapi.json`),
  `every_json_route_is_documented` (read the access matrix table — or the router's route
  list if simpler — and compare its `/api`, `/auth`, `/users`, legacy rows with the
  document's paths+methods, both directions), `ui_and_websocket_are_not_documented`,
  `prefix_reaches_servers_and_docs` (with `COCAT_PROXY_PREFIX=/cocat`).
- [ ] Red, implement, gate. README: a short "API documentation" section (`/docs`,
  `/openapi.json`, public, same as Python). Commit "Serve an OpenAPI document and Swagger UI".

### Task 2: schemathesis

- [ ] With a release build on a scratch port and directory under the scratchpad, seed a
  superuser, log in, and run `schemathesis run http://127.0.0.1:<port>/openapi.json
  --checks all` with the session cookie (install schemathesis with `uv tool run` or a
  scratch venv; never into the Python test checkout's venv). Every failure is a finding:
  either the document is wrong (fix the annotation) or the server is (a reproducer test in
  the module's test file, then the fix), one commit each.
- [ ] Paste the final schemathesis summary into
  `docs/superpowers/reports/2026-11-06-phase-4d.md` with the list of findings and fixes.
  Commit "Report the schemathesis run".
