# Phase 7b — Prometheus metrics — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Export the server's metrics in the Prometheus text format on a private listen
address, as the spec lists them.

**Architecture:** `metrics` 0.24 as the facade everywhere; `metrics-exporter-prometheus`
0.18 (feature `http-listener`) installed once in `serve` when `COCAT_METRICS_BIND` is set.
A new `src/telemetry.rs` owns the metric names (constants), the install function, an axum
middleware for the HTTP instruments (route = `MatchedPath`), and the 60 s sampler for the
database and disk gauges. Instruments elsewhere are one-line calls to helpers in that
module, so names live in one place.

**Tech Stack:** `metrics = "0.24"`, `metrics-exporter-prometheus = { version = "0.18",
default-features = false, features = ["http-listener"] }`. Check they resolve with the
workspace's tokio/hyper before writing code; if `http-listener` pulls a second hyper major,
serve the rendered text from our own small axum router on the bind address instead
(`PrometheusBuilder::install_recorder()` + `handle.render()`), and say which you chose.

**Spec:** `docs/superpowers/specs/2026-11-06-metrics-design.md` (approved 2026-11-06).

## Global Constraints

- Repo `cocat-rs`, branch `main`, commit per task, never push. ONE cargo command at a time,
  foreground. TDD. Full gate before each commit.
- Unset or empty `COCAT_METRICS_BIND`: no recorder, no listener (the `metrics` macros are
  no-ops then). An unparsable value stops the server at startup and names the variable, like
  the other variables.
- Labels: only the values listed in the spec. Never a user id, email, room id, or a raw URI.
  Unmatched routes (404) are recorded with `route="unmatched"`.
- Websocket hot path: counters only, label values are `&'static str`.
- The recorder is process-global: tests that assert on metrics install it once
  (`OnceLock`) and compare deltas, never absolute values.

## Review Focus

1. A concrete room id or email ending up in a label (check every `counter!`/`histogram!`).
2. The exporter reachable through the public port or the proxy.
3. Per-frame allocation or locking added to `ws.rs`/`room.rs`.
4. The sampler running a query or `stat` per scrape instead of every 60 s.
5. Startup behaviour with a bad `COCAT_METRICS_BIND`.

---

### Task 1: Telemetry module, HTTP and room instruments, exporter

**Files:** `crates/cocat-server/Cargo.toml`, `src/telemetry.rs` (new), `src/lib.rs`,
`src/cli.rs` (install + sampler in `serve`), `src/ws.rs` (middleware on the router;
websocket counters), `src/room.rs` (open/flush histograms, rooms gauge, close reasons,
refusals), `src/ratelimit.rs` (refusal counter), `src/mail.rs` (mail counter), README
(variable, advice, a sample `scrape_configs` entry), tests `tests/telemetry.rs`.

**Interfaces (Produces):**
```rust
pub fn install(bind: SocketAddr) -> Result<(), TelemetryError>;   // once, in serve
pub fn bind_from_env(env: &dyn Fn(&str) -> Option<String>) -> Result<Option<SocketAddr>, TelemetryError>;
pub async fn track_http(req: Request, next: Next) -> Response;     // axum middleware
pub fn spawn_sampler(state: AppState) -> JoinHandle<()>;           // every 60 s
// thin helpers used by the hot paths, names as in the spec:
pub fn ws_connected(delta: i64); pub fn ws_frame(direction: Direction, bytes: usize);
pub fn ws_closed(reason: &'static str); pub fn room_opened(seconds: f64);
pub fn room_flushed(seconds: f64); pub fn refused(kind: &'static str); pub fn mail(outcome: &'static str);
```

- [ ] Tests first: `bind_from_env` (unset/empty → None, `127.0.0.1:9187` → Some, garbage →
  error naming `COCAT_METRICS_BIND`); with the recorder installed, after one API request,
  one UI page, one websocket session that syncs and closes, one cold room open and one rate
  limit refusal, the rendered text contains every metric name of the spec, the route label
  is the template (`/api/rooms/{room}`) and no label value contains the room id or an
  email used in the test.
- [ ] Implement, README, gate, commit "Export Prometheus metrics".
