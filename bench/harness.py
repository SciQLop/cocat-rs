#!/usr/bin/env python3
"""Measure a cocat server on the scenarios the spike gate defines.

The clients are raw websockets that decode nothing: each room keeps the send
times in order and a received frame takes the oldest one, because a server may
re-encode an update instead of rebroadcasting the exact bytes it received.
"""

import argparse
import json
import os
import shlex
import signal
import socket
import subprocess
import threading
import time
from collections import deque
from dataclasses import dataclass
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import Request, urlopen
from uuid import uuid4

from pycrdt import Doc, Map, YMessageType, YSyncMessageType, create_sync_message
from pycrdt import create_update_message, handle_sync_message
from websockets.exceptions import ConnectionClosed
from websockets.sync.client import connect

COOKIE = "fastapiusersauth"
BIG_ROOM = "bench100k"
SMALL_ROOM = "bench1k"
METRICS = (
    "rss_peak_mb",
    "rss_end_mb",
    "rss_start_mb",
    "cpu_seconds",
    "open_p50_ms",
    "open_p95_ms",
    "fanout_p50_ms",
    "fanout_p99_ms",
    "updates_per_s",
    "single_room_updates_per_s",
    "lost_updates",
    "duration_s",
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
    "scaleout": Scenario(clients=8, rooms=8, duration=60, rate=10, connects=0),
    "soak": Scenario(clients=50, rooms=1, duration=3600, rate=1, connects=0),
    "openclose": Scenario(clients=0, rooms=0, duration=0, rate=0, connects=20),
}
SMOKE = Scenario(clients=2, rooms=2, duration=2, rate=10, connects=2)


def percentile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = round(fraction / 100 * (len(ordered) - 1))
    return round(ordered[min(max(index, 0), len(ordered) - 1)], 3)


class Server:
    """A spawned server, with its RSS, CPU and readiness probes."""

    def __init__(self, command: list[str], port: int, update_dir: str, db_path: str):
        self.command = command
        self.host = "127.0.0.1"
        self.port = port
        self.update_dir = update_dir
        self.db_path = db_path
        self.process: subprocess.Popen | None = None
        self.rss_start_mb = 0.0
        self._rss_mb = 0.0
        self._rss_peak_mb = 0.0
        self._cpu_seconds = 0.0
        self._samples = threading.Event()
        self.cpu_start = 0.0

    @property
    def base_url(self) -> str:
        return f"http://{self.host}:{self.port}"

    def start(self) -> None:
        self.process = subprocess.Popen(
            [
                *self.command,
                "--host",
                self.host,
                "--port",
                str(self.port),
                "--update_dir",
                self.update_dir,
                "--db_path",
                self.db_path,
            ],
            env=os.environ.copy(),
        )
        self._wait_ready()
        self.rss_start_mb = self.rss_mb()
        self.cpu_start = self.cpu_seconds()
        threading.Thread(target=self._sample, daemon=True).start()

    def _wait_ready(self) -> None:
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise SystemExit(f"the server exited with {self.process.returncode}")
            try:
                urlopen(self.base_url + "/", timeout=1)
            except (URLError, ConnectionError):
                time.sleep(0.1)
            except HTTPError:
                return
            else:
                return

    def _sample(self) -> None:
        while not self._samples.wait(5):
            self._rss_mb = self.rss_mb()
            self._rss_peak_mb = max(self._rss_peak_mb, self._rss_mb)
            self._cpu_seconds = self.cpu_seconds()

    def _status(self, field: str) -> float:
        pid = self.process.pid
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith(f"{field}:"):
                return int(line.split()[1]) / 1024
        raise SystemExit(f"no {field} in /proc/{pid}/status")

    def rss_mb(self) -> float:
        try:
            return self._status("VmRSS")
        except (FileNotFoundError, ProcessLookupError):
            return self._rss_mb

    def peak_rss_mb(self) -> float:
        try:
            return max(self._rss_peak_mb, self._status("VmHWM"))
        except (FileNotFoundError, ProcessLookupError):
            return self._rss_peak_mb

    def cpu_seconds(self) -> float:
        try:
            fields = Path(f"/proc/{self.process.pid}/stat").read_text().split()
        except (FileNotFoundError, ProcessLookupError):
            return self._cpu_seconds
        ticks = int(fields[13]) + int(fields[14])
        return ticks / os.sysconf("SC_CLK_TCK")

    def stop(self) -> tuple[float, float]:
        self._samples.set()
        self._rss_peak_mb = self.peak_rss_mb()
        self._rss_mb = self.rss_mb()
        self._cpu_seconds = self.cpu_seconds()
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGINT)
            self.process.wait(timeout=60)
        return self._rss_mb, self._cpu_seconds


def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def login(server: Server, email: str, password: str) -> str:
    data = urlencode({"username": email, "password": password}).encode()
    request = Request(server.base_url + "/auth/jwt/login", data=data, method="POST")
    with urlopen(request, timeout=10) as response:
        headers = response.headers.get_all("set-cookie", [])
    for header in headers:
        name, _, rest = header.partition("=")
        if name == COOKIE:
            return rest.split(";", 1)[0]
    raise SystemExit(f"no {COOKIE} cookie for {email}")


def open_socket(server: Server, room: str, cookie: str):
    return connect(
        f"ws://{server.host}:{server.port}/room/{room}",
        additional_headers={"cookie": f"{COOKIE}={cookie}"},
        open_timeout=10,
        legacy=True,
    )


def token_update(token: str) -> bytes:
    document = Doc()
    content = document.get("content", type=Map)
    content[token] = token
    return create_update_message(document.get_update())


def tokens_in(socket) -> set[str]:
    socket.send(create_sync_message(Doc()))
    for _ in range(10):
        frame = socket.recv(timeout=10)
        if frame[0] == YMessageType.SYNC and frame[1] == YSyncMessageType.SYNC_STEP2:
            document = Doc()
            document.get("content", type=Map)
            handle_sync_message(frame[1:], document)
            content = document.get("content", type=Map)
            return {str(key) for key in content.keys()}
    return set()


def receive_loop(socket, pending: deque[float], latencies: list[float], stop) -> None:
    while not stop.is_set():
        try:
            frame = socket.recv(timeout=0.2)
        except TimeoutError:
            continue
        except ConnectionClosed:
            return
        if pending:
            latencies.append((time.perf_counter() - pending.popleft()) * 1000)


def run_open(server: Server, cookie: str, connects: int) -> dict:
    latencies = []
    for _ in range(connects):
        started = time.perf_counter()
        socket = open_socket(server, BIG_ROOM, cookie)
        socket.recv()
        latencies.append((time.perf_counter() - started) * 1000)
        socket.close()
    return {
        "open_p50_ms": percentile(latencies, 50),
        "open_p95_ms": percentile(latencies, 95),
    }


def run_openclose(server: Server, cookie: str, cycles: int) -> dict:
    started = time.perf_counter()
    run_open(server, cookie, cycles)
    return {"duration_s": round(time.perf_counter() - started, 3)}


def run_rooms(
    server: Server,
    cookie: str,
    rooms: list[str],
    clients: int,
    duration: float,
    rate: float,
) -> dict:
    latencies: list[float] = []
    pending: dict[str, list[deque[float]]] = {room: [] for room in rooms}
    tokens: dict[str, set[str]] = {room: set() for room in rooms}
    run_id = uuid4().hex[:8]
    stop = threading.Event()
    receivers = []
    sockets = []
    editors = []
    for room in rooms:
        members = [open_socket(server, room, cookie) for _ in range(clients)]
        for socket in members:
            socket.recv()
        for socket in members:
            queue: deque[float] = deque()
            pending[room].append(queue)
            thread = threading.Thread(
                target=receive_loop, args=(socket, queue, latencies, stop), daemon=True
            )
            thread.start()
            receivers.append(thread)
        sockets.extend(members)
        editor = open_socket(server, room, cookie)
        editor.recv()
        sockets.append(editor)
        editors.append(editor)

    started = time.perf_counter()
    index = 0
    next_at = started
    while time.perf_counter() - started < duration:
        for room, editor in zip(rooms, editors, strict=True):
            token = f"{run_id}-{index}"
            index += 1
            tokens[room].add(token)
            update = token_update(token)
            sent_at = time.perf_counter()
            for queue in pending[room]:
                queue.append(sent_at)
            editor.send(update)
        next_at += 1 / rate
        delay = next_at - time.perf_counter()
        if delay > 0:
            time.sleep(delay)
    elapsed = time.perf_counter() - started
    stop.set()
    for thread in receivers:
        thread.join(timeout=1)
    for socket in sockets:
        socket.close()

    lost = 0
    for room in rooms:
        probe = open_socket(server, room, cookie)
        probe.recv()
        present = tokens_in(probe)
        lost += len(tokens[room] - present)
        probe.close()
    return {
        "fanout_p50_ms": percentile(latencies, 50),
        "fanout_p99_ms": percentile(latencies, 99),
        "updates_per_s": round(index / elapsed, 3),
        "lost_updates": lost,
        "duration_s": round(elapsed, 3),
    }


def scaleout_rooms(count: int) -> list[str]:
    if count == 1:
        return [SMALL_ROOM]
    return [f"{SMALL_ROOM}-{index}" for index in range(count)]


def measure(name: str, server: Server, scenario: Scenario, cookie: str) -> dict:
    metrics = {key: None for key in METRICS}
    metrics["scenario"] = name
    if name == "open":
        metrics.update(run_open(server, cookie, scenario.connects))
    elif name == "openclose":
        metrics.update(run_openclose(server, cookie, scenario.connects))
    elif name == "fanout":
        metrics.update(
            run_rooms(
                server,
                cookie,
                [SMALL_ROOM],
                scenario.clients,
                scenario.duration,
                scenario.rate,
            )
        )
    elif name == "scaleout":
        single = run_rooms(
            server,
            cookie,
            scaleout_rooms(1),
            scenario.clients,
            scenario.duration,
            scenario.rate,
        )
        many = run_rooms(
            server,
            cookie,
            scaleout_rooms(scenario.rooms),
            scenario.clients,
            scenario.duration,
            scenario.rate,
        )
        metrics.update(many)
        metrics["single_room_updates_per_s"] = single["updates_per_s"]
    elif name == "soak":
        metrics.update(
            run_rooms(
                server,
                cookie,
                [SMALL_ROOM],
                scenario.clients,
                scenario.duration,
                scenario.rate,
            )
        )
    rss_end, cpu_end = server.stop()
    metrics["rss_end_mb"] = round(rss_end, 3)
    metrics["rss_peak_mb"] = round(server.peak_rss_mb(), 3)
    metrics["rss_start_mb"] = round(server.rss_start_mb, 3)
    metrics["cpu_seconds"] = round(cpu_end - server.cpu_start, 3)
    return metrics


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scenario", choices=sorted(SCENARIOS))
    parser.add_argument("--server-cmd", required=True)
    parser.add_argument("--db", required=True, type=Path)
    parser.add_argument("--update-dir", required=True, type=Path)
    parser.add_argument("--email", required=True)
    parser.add_argument("--password", required=True)
    parser.add_argument("--json", type=Path)
    parser.add_argument("--smoke", action="store_true")
    arguments = parser.parse_args()

    scenario = SMOKE if arguments.smoke else SCENARIOS[arguments.scenario]
    server = Server(
        shlex.split(arguments.server_cmd),
        free_port(),
        str(arguments.update_dir),
        str(arguments.db),
    )
    server.start()
    try:
        cookie = login(server, arguments.email, arguments.password)
        metrics = measure(arguments.scenario, server, scenario, cookie)
    finally:
        server.stop()
    if arguments.json is not None:
        arguments.json.parent.mkdir(parents=True, exist_ok=True)
        arguments.json.write_text(json.dumps(metrics, indent=2) + "\n")
    print(json.dumps(metrics))


if __name__ == "__main__":
    main()
