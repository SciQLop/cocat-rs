#!/usr/bin/env python3
"""Measure a cocat server on the scenarios the spike gate defines.

Clients are raw websockets: an edit is a real cocat event change produced with the
`cocat` model, and each room's clients run in their own process so the harness's own
GIL is not what the latencies measure. Senders and receivers of one room share a
process, so `time.perf_counter()` is the same clock for both.
"""

import argparse
import json
import multiprocessing
import os
import resource
import shlex
import signal
import socket
import subprocess
import threading
import time
from collections import deque
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import Request, urlopen

from cocat import DB
from pycrdt import Doc, YMessageType, YSyncMessageType, create_sync_message
from pycrdt import create_update_message, handle_sync_message
from websockets.exceptions import ConnectionClosed
from websockets.sync.client import connect

COOKIE = "fastapiusersauth"
BIG_ROOM = "bench100k"
SMALL_ROOM = "bench1k"
IN_FLIGHT = 4
UNLOAD_TIMEOUT = 20.0
OBSERVABLE_GROWTH_MB = 100.0
SOAK_SAMPLE_AT = 300.0
EPOCH = datetime(2020, 1, 1, tzinfo=timezone.utc)
METRICS = (
    "rss_peak_mb",
    "rss_end_mb",
    "rss_start_mb",
    "rss_open_mb",
    "rss_before_mb",
    "rss_after_mb",
    "rss_5min_mb",
    "rss_drift_pct",
    "cpu_seconds",
    "open_p50_ms",
    "open_p95_ms",
    "fanout_p50_ms",
    "fanout_p99_ms",
    "updates_per_s",
    "per_room_updates_per_s",
    "single_room_updates_per_s",
    "lost_updates",
    "lost_updates_after_restart",
    "unload_timeouts",
    "duration_s",
    "harness_cpu_seconds",
    "harness_peak_core_fraction",
)


@dataclass(frozen=True)
class Scenario:
    clients: int
    rooms: int
    duration: float
    rate: float
    connects: int


SCENARIOS = {
    "open": Scenario(clients=0, rooms=0, duration=0, rate=0, connects=20),
    "fanout": Scenario(clients=50, rooms=1, duration=300, rate=1, connects=0),
    "scaleout": Scenario(clients=8, rooms=8, duration=60, rate=1, connects=0),
    "soak": Scenario(clients=8, rooms=8, duration=3600, rate=1, connects=0),
    "openclose": Scenario(clients=0, rooms=0, duration=0, rate=0, connects=20),
}
SMOKE = Scenario(clients=2, rooms=2, duration=2, rate=1, connects=2)


def room_set(count: int) -> list[str]:
    return [f"{SMALL_ROOM}-{index}" for index in range(count)]


def scenario_rooms(name: str) -> list[str]:
    if name in ("open", "openclose", "fanout"):
        return [BIG_ROOM]
    if name == "soak":
        return room_set(8)
    return [SMALL_ROOM, *room_set(8)]


def percentile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = round(fraction / 100 * (len(ordered) - 1))
    return round(ordered[index], 3)


def drift_pct(start: float | None, end: float | None) -> float | None:
    if not start or end is None:
        return None
    return round((end - start) / start * 100, 3)


class Server:
    def __init__(self, command: list[str], update_dir: Path, db_path: Path) -> None:
        self.command = command
        self.host = "127.0.0.1"
        self.port = free_port()
        self.update_dir = update_dir
        self.db_path = db_path
        self.process: subprocess.Popen | None = None
        self.rss_start_mb = 0.0
        self._rss_mb = 0.0
        self._rss_peak_mb = 0.0
        self._cpu_seconds = 0.0
        self.cpu_start = 0.0
        self._samples = threading.Event()
        self._sampler: threading.Thread | None = None

    @property
    def base_url(self) -> str:
        return f"http://{self.host}:{self.port}"

    def start(self) -> None:
        self.port = free_port()
        self.process = subprocess.Popen(
            [
                *self.command,
                "--host",
                self.host,
                "--port",
                str(self.port),
                "--update_dir",
                str(self.update_dir),
                "--db_path",
                str(self.db_path),
            ]
        )
        self._wait_ready()
        self.rss_start_mb = self.rss_mb
        self.cpu_start = process_cpu_seconds(self.process.pid)
        self._rss_mb = 0.0
        self._rss_peak_mb = 0.0
        self._cpu_seconds = 0.0
        self._samples = threading.Event()
        self._sampler = threading.Thread(target=self._sample, daemon=True)
        self._sampler.start()

    def restart(self) -> tuple[float, float, float]:
        stopped = self.stop()
        self.start()
        return stopped

    def _wait_ready(self) -> None:
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if self.process is not None and self.process.poll() is not None:
                raise SystemExit(f"the server exited with {self.process.returncode}")
            try:
                with urlopen(self.base_url, timeout=1):
                    return
            except HTTPError:
                return
            except URLError:
                time.sleep(0.05)
        raise SystemExit("the server did not answer within 30 s")

    def _sample(self) -> None:
        while not self._samples.wait(5):
            self._rss_mb = self._read_rss()
            self._rss_peak_mb = max(self._rss_peak_mb, self._rss_mb)
            self._cpu_seconds = process_cpu_seconds(self.process.pid) - self.cpu_start

    def _status(self, field: str) -> int:
        assert self.process is not None
        try:
            text = Path(f"/proc/{self.process.pid}/status").read_text()
        except FileNotFoundError:
            return 0
        for line in text.splitlines():
            if line.startswith(f"{field}:"):
                return int(line.split()[1])
        return 0

    def _read_rss(self) -> float:
        return self._status("VmRSS") / 1024

    @property
    def rss_mb(self) -> float:
        return self._read_rss() or self._rss_mb

    @property
    def peak_rss_mb(self) -> float:
        return max(self._status("VmHWM") / 1024, self._rss_peak_mb)

    def stop(self) -> tuple[float, float, float]:
        self._samples.set()
        if self._sampler is not None:
            self._sampler.join(timeout=1)
        rss = self.rss_mb or self._rss_mb
        peak = self.peak_rss_mb
        cpu = (
            process_cpu_seconds(self.process.pid) - self.cpu_start
        ) or self._cpu_seconds
        if self.process is not None and self.process.poll() is None:
            self.process.send_signal(signal.SIGINT)
            self.process.wait(timeout=60)
        return rss, peak, cpu


def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def self_cpu_seconds() -> float:
    usage = resource.getrusage(resource.RUSAGE_SELF)
    return usage.ru_utime + usage.ru_stime


def process_cpu_seconds(pid: int) -> float:
    try:
        stat = Path(f"/proc/{pid}/stat").read_text()
    except FileNotFoundError:
        return 0.0
    fields = stat.rsplit(")", 1)[1].split()
    ticks = float(fields[11]) + float(fields[12])
    return ticks / os.sysconf("SC_CLK_TCK")


def login(server: Server, email: str, password: str) -> str:
    data = urlencode({"username": email, "password": password}).encode()
    request = Request(server.base_url + "/auth/jwt/login", data=data, method="POST")
    with urlopen(request, timeout=10) as response:
        for header in response.headers.get_all("set-cookie") or []:
            name, _, value = header.partition("=")
            if name == COOKIE:
                return value.split(";")[0]
    raise SystemExit("the login response carried no session cookie")


def open_socket(host: str, port: int, room: str, cookie: str):
    return connect(
        f"ws://{host}:{port}/room/{room}",
        additional_headers={"cookie": f"{COOKIE}={cookie}"},
        open_timeout=10,
        # A synced 100k-event room is one 37.8 MB SyncStep2 frame, far past the 1 MiB
        # default; the gate measures that frame, so the harness must accept it.
        max_size=None,
        legacy=True,
    )


def sync_into(document: Doc, socket) -> None:
    socket.send(create_sync_message(document))
    while True:
        frame = socket.recv(timeout=10)
        if frame[0] == YMessageType.SYNC and frame[1] == YSyncMessageType.SYNC_STEP2:
            handle_sync_message(frame[1:], document)
            return


def fresh_database(socket) -> DB:
    document = Doc()
    sync_into(document, socket)
    return DB(doc=document)


@dataclass
class Participant:
    index: int
    socket: object
    database: DB
    event_ids: list[str]


def participants(host: str, port: int, room: str, cookie: str, clients: int) -> list:
    """One socket per client, one `Doc` per room.

    A synced 100k-event room costs ~540 MB in pycrdt, so 50 independent client docs
    would need ~27 GB; the sockets share the doc while each still edits its own slice
    and receives every broadcast. The server sees 50 connections, which is what it is
    measured on.
    """
    first = open_socket(host, port, room, cookie)
    database = fresh_database(first)
    event_ids = sorted(str(event.uuid) for event in database.events)
    sockets = [first] + [
        open_socket(host, port, room, cookie) for _ in range(clients - 1)
    ]
    for connection in sockets[1:]:
        sync_into(database.doc, connection)
    return [
        Participant(index, connection, database, event_ids[index::clients])
        for index, connection in enumerate(sockets)
    ]


def edit(participant: Participant, tick: int) -> tuple[bytes, str, datetime]:
    event_id = participant.event_ids[tick % len(participant.event_ids)]
    value = EPOCH + timedelta(seconds=tick * 1_000_000 + participant.index)
    state = participant.database.doc.get_state()
    participant.database.get_event(event_id).stop = value
    return participant.database.doc.get_update(state), event_id, value


def receive_loop(
    socket, pending: deque[float], latencies: list[float], stop, counters, index
):
    while not stop.is_set():
        try:
            socket.recv(timeout=0.2)
        except TimeoutError:
            continue
        except ConnectionClosed:
            return
        counters[index] += 1
        if pending:
            latencies.append((time.perf_counter() - pending.popleft()) * 1000)


def run_room(
    host: str,
    port: int,
    room: str,
    cookie: str,
    clients: int,
    duration: float,
    rate: float,
    closed_loop: bool,
    in_flight: int,
    results,
) -> None:
    started_cpu = self_cpu_seconds()
    opened = participants(host, port, room, cookie, clients)
    pending = [deque() for _ in opened]
    latencies: list[float] = []
    counters = [0] * len(opened)
    stop = threading.Event()
    threads = [
        threading.Thread(
            target=receive_loop,
            args=(participant.socket, pending[index], latencies, stop, counters, index),
            daemon=True,
        )
        for index, participant in enumerate(opened)
    ]
    for thread in threads:
        thread.start()

    written: dict[str, datetime] = {}
    sent = 0
    started = time.perf_counter()
    tick = 0
    while time.perf_counter() - started < duration:
        if closed_loop:
            if sent - counters[0] >= in_flight:
                time.sleep(0.001)
                continue
            send_edit(opened[0], tick, pending, written)
            sent += 1
        else:
            for participant in opened:
                due = started + (tick + (participant.index + 1) / len(opened)) / rate
                delay = due - time.perf_counter()
                if delay > 0:
                    time.sleep(delay)
                send_edit(participant, tick, pending, written)
                sent += 1
            tick += 1
    elapsed = time.perf_counter() - started
    stop.set()
    for thread in threads:
        thread.join(timeout=1)
    for participant in opened:
        participant.socket.close()

    payload = {
        "room": room,
        "latencies": latencies,
        "delivered": counters[0],
        "sent": sent,
        "written": written,
        "duration": elapsed,
        "cpu_seconds": self_cpu_seconds() - started_cpu,
    }
    results.send(payload)
    results.close()


def send_edit(
    participant: Participant,
    tick: int,
    pending: list[deque[float]],
    written: dict[str, datetime],
) -> None:
    update, event_id, value = edit(participant, tick)
    written[event_id] = value
    sent_at = time.perf_counter()
    for queue in pending:
        queue.append(sent_at)
    participant.socket.send(create_update_message(update))


def probe_written(
    host: str, port: int, room: str, cookie: str, written: dict[str, datetime]
) -> int:
    socket = open_socket(host, port, room, cookie)
    database = fresh_database(socket)
    socket.close()
    return sum(
        1
        for event_id, value in written.items()
        if database.get_event(event_id).stop != value
    )


def probe_rooms(server: Server, cookie: str, written: list[tuple[str, dict]]) -> int:
    return sum(
        probe_written(server.host, server.port, room, cookie, values)
        for room, values in written
    )


def start_rooms(server, cookie, rooms, clients, duration, rate, closed_loop, in_flight):
    context = multiprocessing.get_context("fork")
    running = []
    for room in rooms:
        receiver, sender = context.Pipe(duplex=False)
        process = context.Process(
            target=run_room,
            args=(
                server.host,
                server.port,
                room,
                cookie,
                clients,
                duration,
                rate,
                closed_loop,
                in_flight,
                sender,
            ),
            daemon=True,
        )
        process.start()
        sender.close()
        running.append((process, receiver))
    return running


def collect(server, running, duration, sample_at):
    started = time.monotonic()
    measured: list = [None] * len(running)
    sampled = None
    deadline = started + duration + 120
    while any(result is None for result in measured):
        if time.monotonic() > deadline:
            raise SystemExit("a room process did not report; is the server up?")
        if (
            sample_at is not None
            and sampled is None
            and time.monotonic() - started >= sample_at
        ):
            sampled = server.rss_mb
        for index, (_, receiver) in enumerate(running):
            if measured[index] is None and receiver.poll(0.05):
                measured[index] = receiver.recv()
    for process, receiver in running:
        receiver.close()
        process.join(timeout=30)
    return measured, sampled


def run_rooms(
    server: Server,
    cookie: str,
    rooms: list[str],
    clients: int,
    duration: float,
    rate: float,
    closed_loop: bool,
    in_flight: int,
    sample_at: float | None = None,
    probe: bool = True,
) -> dict:
    running = start_rooms(
        server, cookie, rooms, clients, duration, rate, closed_loop, in_flight
    )
    measured, sampled = collect(server, running, duration, sample_at)

    latencies = [latency for room in measured for latency in room["latencies"]]
    elapsed = max(room["duration"] for room in measured)
    delivered = sum(room["delivered"] for room in measured)
    cpu = sum(room["cpu_seconds"] for room in measured)
    written = [(room["room"], room["written"]) for room in measured]
    metrics = {
        "fanout_p50_ms": percentile(latencies, 50),
        "fanout_p99_ms": percentile(latencies, 99),
        "updates_per_s": round(delivered / elapsed, 3),
        "per_room_updates_per_s": round(delivered / elapsed / len(measured), 3),
        "lost_updates": probe_rooms(server, cookie, written) if probe else None,
        "duration_s": round(elapsed, 3),
        "harness_cpu_seconds": round(cpu, 3),
        "harness_peak_core_fraction": round(
            max(room["cpu_seconds"] for room in measured) / elapsed, 3
        ),
    }
    if not probe:
        metrics["written"] = written
    if sampled is not None:
        metrics["rss_sample_mb"] = sampled
    return metrics


def wait_unloaded(server: Server, baseline_mb: float, open_mb: float) -> bool | None:
    """RSS is the only signal a server gives that it dropped a room: wait for most
    of the growth to go away. Polling, because there is no event to await. A room
    too small to move RSS by OBSERVABLE_GROWTH_MB cannot be watched this way.
    """
    if open_mb - baseline_mb < OBSERVABLE_GROWTH_MB:
        return None
    target = baseline_mb + (open_mb - baseline_mb) / 2
    deadline = time.monotonic() + UNLOAD_TIMEOUT
    while time.monotonic() < deadline:
        if server.rss_mb <= target:
            return True
        time.sleep(0.05)
    return False


def open_cycles(server: Server, cookie: str, cycles: int) -> dict:
    # The process start RSS is the only baseline that says whether a cycle returned
    # its memory: a server that never unloads leaves the warm-up's room resident, so
    # measuring the next cycle against that inflated figure shows no growth at all.
    start = server.rss_start_mb
    warmup = open_socket(server.host, server.port, BIG_ROOM, cookie)
    warmup.recv()
    warmup_open = server.rss_mb
    warmup.close()
    wait_unloaded(server, start, warmup_open)

    before = server.rss_mb
    latencies = []
    open_rss = []
    timeouts = 0
    observed = 0
    started = time.perf_counter()
    for _ in range(cycles):
        at = time.perf_counter()
        socket = open_socket(server.host, server.port, BIG_ROOM, cookie)
        socket.recv()
        latencies.append((time.perf_counter() - at) * 1000)
        open_rss.append(server.rss_mb)
        socket.close()
        waited = wait_unloaded(server, start, open_rss[-1])
        if waited is not None:
            observed += 1
            if not waited:
                timeouts += 1
    return {
        "latencies": latencies,
        "rss_open_mb": max(open_rss),
        "rss_before_mb": round(before, 3),
        "rss_after_mb": round(server.rss_mb, 3),
        "unload_timeouts": timeouts if observed else None,
        "duration_s": round(time.perf_counter() - started, 3),
    }


def run_soak(server: Server, cookie: str, scenario: Scenario, in_flight: int) -> dict:
    rss_start = server.rss_start_mb
    rooms = room_set(scenario.rooms)
    measured = run_rooms(
        server,
        cookie,
        rooms,
        scenario.clients,
        scenario.duration,
        scenario.rate,
        closed_loop=False,
        in_flight=in_flight,
        sample_at=SOAK_SAMPLE_AT,
        probe=False,
    )
    written = measured.pop("written", [])
    rss_5min = measured.pop("rss_sample_mb", None)
    rss_end = server.rss_mb
    lost = probe_rooms(server, cookie, written)
    _, peak, cpu = server.restart()
    lost_after_restart = probe_rooms(server, cookie, written)
    return {
        **measured,
        "rss_start_mb": round(rss_start, 3),
        "rss_5min_mb": round(rss_5min, 3) if rss_5min is not None else None,
        "rss_end_mb": round(rss_end, 3),
        "rss_peak_mb": round(peak, 3),
        "rss_drift_pct": drift_pct(rss_5min, rss_end),
        "cpu_seconds": round(cpu, 3),
        "lost_updates": lost,
        "lost_updates_after_restart": lost_after_restart,
    }


def measure(
    name: str, server: Server, scenario: Scenario, cookie: str, in_flight: int
) -> dict:
    metrics = {key: None for key in METRICS}
    metrics["scenario"] = name
    if name in ("open", "openclose"):
        cycles = open_cycles(server, cookie, scenario.connects)
        metrics["unload_timeouts"] = cycles["unload_timeouts"]
        if name == "open":
            metrics["open_p50_ms"] = percentile(cycles["latencies"], 50)
            metrics["open_p95_ms"] = percentile(cycles["latencies"], 95)
            metrics["rss_open_mb"] = round(cycles["rss_open_mb"], 3)
        else:
            metrics["duration_s"] = cycles["duration_s"]
            metrics["rss_before_mb"] = cycles["rss_before_mb"]
            metrics["rss_after_mb"] = cycles["rss_after_mb"]
            metrics["rss_drift_pct"] = drift_pct(
                cycles["rss_before_mb"], cycles["rss_after_mb"]
            )
    elif name == "fanout":
        metrics.update(
            run_rooms(
                server,
                cookie,
                [BIG_ROOM],
                scenario.clients,
                scenario.duration,
                scenario.rate,
                closed_loop=False,
                in_flight=in_flight,
            )
        )
    elif name == "soak":
        metrics.update(run_soak(server, cookie, scenario, in_flight))
    elif name == "scaleout":
        single = run_rooms(
            server,
            cookie,
            [SMALL_ROOM],
            scenario.clients,
            scenario.duration,
            scenario.rate,
            closed_loop=True,
            in_flight=in_flight,
        )
        many = run_rooms(
            server,
            cookie,
            room_set(scenario.rooms),
            scenario.clients,
            scenario.duration,
            scenario.rate,
            closed_loop=True,
            in_flight=in_flight,
        )
        metrics.update(many)
        metrics["single_room_updates_per_s"] = single["updates_per_s"]
    rss_end, rss_peak, cpu_end = server.stop()
    for key, value in (
        ("rss_end_mb", rss_end),
        ("rss_peak_mb", rss_peak),
        ("rss_start_mb", server.rss_start_mb),
        ("cpu_seconds", cpu_end),
    ):
        if metrics[key] is None:
            metrics[key] = round(value, 3)
    return metrics


def check_rooms(update_dir: Path, rooms: list[str]) -> None:
    for room in rooms:
        path = update_dir / f"{room}.y"
        if not path.exists():
            raise SystemExit(f"{path} is missing; run bench/run_gates.sh")
    size = (update_dir / f"{BIG_ROOM}.y").stat().st_size
    if not 35e6 <= size <= 40e6:
        raise SystemExit(f"{BIG_ROOM}.y is {size / 1e6:.1f} MB, expected 35-40 MB")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scenario", choices=sorted(SCENARIOS))
    parser.add_argument(
        "--server-cmd", required=True, help="server command, e.g. 'cocat serve'"
    )
    parser.add_argument("--db", type=Path, required=True)
    parser.add_argument("--update-dir", type=Path, required=True)
    parser.add_argument("--email", required=True)
    parser.add_argument("--password", required=True)
    parser.add_argument("--json", type=Path)
    parser.add_argument(
        "--in-flight",
        type=int,
        default=IN_FLIGHT,
        help="closed-loop scale-out: updates outstanding per room",
    )
    parser.add_argument(
        "--smoke", action="store_true", help="seconds of traffic, tiny rooms"
    )
    arguments = parser.parse_args()

    scenario = SMOKE if arguments.smoke else SCENARIOS[arguments.scenario]
    if not arguments.smoke:
        check_rooms(arguments.update_dir, scenario_rooms(arguments.scenario))
    server = Server(
        shlex.split(arguments.server_cmd),
        arguments.update_dir,
        arguments.db,
    )
    server.start()
    try:
        cookie = login(server, arguments.email, arguments.password)
        metrics = measure(
            arguments.scenario, server, scenario, cookie, arguments.in_flight
        )
    finally:
        server.stop()
    text = json.dumps(metrics)
    if arguments.json is not None:
        arguments.json.write_text(text + "\n")
    print(text)


if __name__ == "__main__":
    main()
