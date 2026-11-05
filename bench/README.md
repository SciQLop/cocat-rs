# Benchmark harness

Same client stack and the same scenario parameters for the Python baseline and the
Rust spike, so the two JSON files can be compared field by field.

The harness lives with the Rust code because the plan and the docs are here, but it is
a Python client: run it with a venv that has `pycrdt`, `websockets` and `cocat`
installed (`/home/jeandet/Documents/prog/cocat-ui/.venv` at the time of the spike).

## Rooms

`make_room.py` writes a room through cocat's own model (`cocat.DB`, `create_catalogue`,
`create_event` with tags/products/rating), one update per catalogue, so the files hold
what a real session holds rather than a flat map of strings:

```sh
python bench/make_room.py --dir updates --room bench100k --catalogues 100 --events 1000
python bench/make_room.py --dir updates --room bench1k --catalogues 10 --events 100
for index in 0 1 2 3 4 5 6 7; do
  python bench/make_room.py --dir updates --room "bench1k-$index" --catalogues 10 --events 100
done
```

`run_gates.sh` does exactly this once into `bench/work/pristine`.

`bench100k` is about 38 MB; the harness refuses to run a non-smoke scenario when the
file is outside 35-40 MB, because every gate number is about that room.

Seed the database with either CLI (owner `bench@test.org`, password `pwd`, member of
every room):

```sh
cocat create-user --email bench@test.org --password pwd --db_path bench.db
cocat create-room --owner bench@test.org --room-id bench100k --db_path bench.db
```

## Scenarios

| scenario | clients | duration | traffic |
| --- | --- | --- | --- |
| `open` | - | - | 20 cold connects to `bench100k`, each after the previous room unloaded; latency to the first frame, RSS while open |
| `openclose` | - | - | 20 cold connect / receive / disconnect cycles on `bench100k`; RSS after a warm-up cycle and after the last |
| `fanout` | 50 in `bench100k` | 300 s | every client edits one event of its own once per second, staggered; every client receives every edit |
| `scaleout` | 8 per room | 60 s per phase | closed loop: each room's editor keeps 4 updates in flight, 1 room then 8 rooms |
| `soak` | 8 per room, 8 rooms | 3600 s | same traffic as `fanout`, then SIGINT, restart on the same files, probe every edit |

`fanout` and `soak` are open loop: the offered rate is one edit per client per second,
one client every 20 ms. `scaleout` is closed loop on purpose: the editor sends the next
edit only after the broadcast of an earlier one comes back, so `updates_per_s` is what
the server delivered, not what the harness tried to send, and the two servers can
actually differ.

A cold open needs the room to be gone, and neither server reports it, so the harness
waits for RSS to fall back most of the way to the idle baseline (20 s cap). A room too
small to move RSS by 100 MB cannot be watched that way: `unload_timeouts` is then
`null`, and the run says nothing about coldness instead of pretending.

`--in-flight` sets how many updates a room keeps outstanding (default 4). The gate uses
4: one round trip is in flight while the next is on the wire, so a server's per-update
cost shows up in throughput, while a deeper queue would only measure how much work the
server buffers before the client notices.

An edit is a real cocat change: the client sets one event's `stop` and sends the
resulting `sync update`, so the room grows the way it grows under a user.

## Running the gates

`run_gates.sh` builds the pristine room set once (`bench/work/pristine`: `bench100k`
with 100 catalogues x 1000 events, `bench1k` and `bench1k-0..7` with 10 x 100), then for
every scenario copies it into a fresh directory, seeds a fresh database with that
server's own CLI and writes `bench/results/<scenario>-<server>.json`. Scenarios run one
at a time; nothing else runs in the background.

```sh
SERVER=rust bench/run_gates.sh          # target/release/cocat-server, build it first
SERVER=python bench/run_gates.sh        # the cocat-ui venv's `cocat`
python bench/compare.py                 # gate table, Rust next to Python
```

The long runs (fanout 5 min, soak 1 h) take about 1.5 h per server. `--smoke` shrinks
every scenario to seconds and writes `bench/results/smoke-*.json` in a separate
`bench/work/smoke-*` directory, so it never touches gate numbers; `compare.py --smoke`
prints their table.

`compare.py` applies the design's thresholds row by row, lists the hard NO-GO lines the
Rust run crossed, and names the rows the harness itself may have limited
(`harness_peak_core_fraction > 0.7`).

## Running one scenario by hand

```sh
python bench/harness.py fanout \
  --server-cmd "/path/to/cocat serve" \
  --db bench.db --update-dir updates \
  --email bench@test.org --password pwd \
  --json results/fanout-python.json
```

The command already contains the `serve` subcommand; the harness appends `--host
--port --update_dir --db_path`. It starts the server on a free port, logs in over HTTP,
runs the scenario, stops the server with SIGINT and prints one JSON object.

The gate numbers come from the release binary: build it with `cargo build --release -p
cocat-server` and pass `--server-cmd "<repo>/target/release/cocat-server serve"`, as the
Python baseline is its normal install, not a debug run.

`--smoke` replaces every duration with two seconds and two clients/connects and skips
the room-size check; it is there to prove the harness works, not to produce numbers.

## Processes and clocks

Each room's clients run in their own process (`multiprocessing`, fork), so 50 receivers
in one room are not fighting one Python GIL; a scale-out run uses one process per room.
Senders and receivers of a room share the process, so `time.perf_counter()` is the same
clock for the send and the receive of an update, even though it is per-process: it is
monotonic and, on Linux, backed by the same system clock in every process.

The JSON reports the harness's own cost:

- `harness_cpu_seconds` - CPU used by all room processes together.
- `harness_peak_core_fraction` - CPU of the busiest room process divided by the run
  duration. Above ~0.7 the harness process is close to a full core and the latencies
  may describe the client rather than the server: say so instead of quoting them.

## Metrics

| key | meaning |
| --- | --- |
| `rss_start_mb`, `rss_end_mb`, `rss_peak_mb` | server RSS from `/proc`, start / end / high-water mark |
| `rss_open_mb` | `open`: peak server RSS read while `bench100k` is open |
| `rss_before_mb`, `rss_after_mb` | `openclose`: RSS after the warm-up cycle and after the last cycle |
| `rss_5min_mb` | `soak`: server RSS at minute 5 |
| `rss_drift_pct` | `soak`: minute 5 to end; `openclose`: before to after, in % |
| `cpu_seconds` | server CPU time from `/proc`, start to stop |
| `open_p50_ms`, `open_p95_ms` | cold connect to first frame |
| `unload_timeouts` | `open`/`openclose`: cold-open waits that hit the 20 s cap (`null`: room too small to watch) |
| `fanout_p50_ms`, `fanout_p99_ms` | send to local receive, per client |
| `updates_per_s` | delivered update frames per second, one designated receiver |
| `per_room_updates_per_s` | the same divided by the number of rooms |
| `single_room_updates_per_s` | scale-out only: the 1-room phase, same metric |
| `lost_updates` | edits written by the harness that a fresh client does not find in the room at the end |
| `lost_updates_after_restart` | `soak`: the same probe after SIGINT and a restart on the same files |
| `duration_s` | measured traffic window |
| `harness_cpu_seconds`, `harness_peak_core_fraction` | see above |

A key a scenario does not produce is `null`.
