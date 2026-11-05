# The spike benchmark

`make_room.py` writes a room file the way the Python server does, and
`harness.py` measures a server binary on the five gate scenarios. Both servers
are measured with the same client stack and the same scenario parameters.

## Rooms

```sh
.venv/bin/python bench/make_room.py --dir /tmp/spike/updates --room bench100k --catalogues 100 --events 1000
.venv/bin/python bench/make_room.py --dir /tmp/spike/updates --room bench1k --catalogues 10 --events 100
for room in bench1k-0 bench1k-1 bench1k-2 bench1k-3 bench1k-4 bench1k-5 bench1k-6 bench1k-7; do
  .venv/bin/python bench/make_room.py --dir /tmp/spike/updates --room "$room" --catalogues 10 --events 100
done
```

The 100 × 1000 room prints a size near 38 MB. The rooms also need rows in the
database, seeded with either CLI:

```sh
cocat create-user --email bench@test.org --password pwd --db_path /tmp/spike/python.db
cocat create-room --owner bench@test.org --room-id bench100k --db_path /tmp/spike/python.db
```

## Scenarios

| Scenario | Load |
| --- | --- |
| `open` | 20 cold connects to `bench100k` |
| `fanout` | 50 clients and one editor on `bench1k`, 1 update/s for 300 s |
| `scaleout` | 8 clients per room at 10 updates/s, one room then 8 rooms, 60 s each |
| `soak` | 50 clients on `bench1k`, 1 update/s for 3600 s |
| `openclose` | 20 connect / receive / disconnect cycles on `bench100k` |

`--smoke` shrinks every scenario to 2 clients and 2 s (2 connects, 2 cycles,
2 rooms for the scale-out shape).

```sh
COCAT_PUBLIC_URL= .venv/bin/python bench/harness.py fanout --smoke \
  --server-cmd ".venv/bin/cocat serve" \
  --db /tmp/spike/python.db --update-dir /tmp/spike/updates \
  --email bench@test.org --password pwd --json bench/results/python-fanout.json
```

The server command is the binary plus its `serve` subcommand; the harness adds
`--host 127.0.0.1 --port <free port> --update_dir <dir> --db_path <db>`.

## Measurements

Clients are raw websockets that keep no document, so 50 replicas of the 100k
room never fit in memory. A frame is timed by matching the bytes the server
broadcasts against the bytes the editor sent in the same process.

| Key | Meaning |
| --- | --- |
| `rss_start_mb`, `rss_end_mb`, `rss_peak_mb` | server RSS at start, at the end, and its high-water mark |
| `cpu_seconds` | server user + system time for the scenario |
| `open_p50_ms`, `open_p95_ms` | cold connect until the first sync frame |
| `fanout_p50_ms`, `fanout_p99_ms` | editor send until a receiver got the broadcast |
| `updates_per_s` | updates sent / seconds elapsed |
| `single_room_updates_per_s` | the one-room shape of `scaleout`, for the scale-out ratio |
| `lost_updates` | tokens sent minus tokens present in the final room state |
| `duration_s` | wall time of the measured loop |

Every key is always present; the ones a scenario does not produce are `null`.
