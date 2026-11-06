# Metrics export — design

## Why

The admin dashboard shows the state of the server now. Operators also need history and the
numbers the dashboard does not show (latency, traffic, refusals) in the tool they already
use: Grafana. Labs that deploy their own server mostly run Prometheus; the LPP Grafana reads
Graphite.

## Goals

- One set of instruments, recorded through the `metrics` crate facade, exported in the
  Prometheus text format by `metrics-exporter-prometheus` on a listen address of its own.
- `COCAT_METRICS_BIND` (for example `127.0.0.1:9187`): unset means no exporter and no
  listener. Never on the public port, so nothing about the server leaks through the proxy.
- Instruments (names `cocat_*`, labels kept low-cardinality: never a user, email or room id):
  - `cocat_http_requests_total{route, method, status}` and
    `cocat_http_request_duration_seconds{route, method}` (histogram), `route` being the
    matched axum path template (`/api/rooms/{room}`), never the concrete URI.
  - `cocat_ws_connections` (gauge), `cocat_ws_frames_total{direction}`,
    `cocat_ws_bytes_total{direction}`, `cocat_ws_closed_total{reason}` (the close codes the
    server already names: revoked, too_slow, over_budget, too_large, full, failed).
  - `cocat_rooms_open` (gauge), `cocat_room_open_seconds` (histogram, cold open),
    `cocat_room_flush_seconds` (histogram), `cocat_room_file_bytes_total` (gauge, sum of
    the room files, refreshed with the gauge sampling below).
  - `cocat_refusals_total{kind}`: rate_limit, storage_quota, cold_read_busy.
  - `cocat_mail_total{outcome}`: sent, failed.
  - `cocat_users`, `cocat_rooms` (gauges from the database), sampled every 60 s with the
    file total, so a scrape never runs a query or walks the disk.
- LPP's Graphite: the site's existing probe script (`lpp-services-status.py`, which already
  pushes speasy-proxy numbers to Graphite) scrapes `/metrics` and pushes a chosen subset. The
  server stays standard; the site glue stays in the site's tooling.

## Non-goals

- A Graphite pusher inside the server (it would mean re-parsing the exporter's text).
- Per-user or per-room series (cardinality, privacy). The admin pages cover per-room views.
- Tracing/OpenTelemetry spans.
- A Grafana dashboard in this repo (later, as JSON, once real series exist).

## Constraints

- Recording must cost nothing measurable on the websocket hot path: counters only there, no
  allocation per frame, no label built from data.
- A test scrapes the exporter and checks every instrument name above is present after a
  request, a websocket session and a room open.
- README: the variable, the port advice (bind to localhost or a private interface), and a
  sample Prometheus scrape config.
