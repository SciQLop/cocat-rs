import socket
import subprocess
import time
import urllib.parse
import urllib.request
import uuid
from dataclasses import dataclass
from pathlib import Path

import pytest

import cocat_rs as cc

ROOT = Path(__file__).resolve().parents[3]
BINARY = ROOT / "target" / "debug" / "cocat-server"
PASSWORD = "plum orbit lantern cello"


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait_for_port(port: int, timeout: float = 15.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.02)
    raise TimeoutError(f"the server did not listen on {port}")


def run(*arguments: str) -> None:
    subprocess.run([str(BINARY), *arguments], check=True, capture_output=True)


@dataclass
class Server:
    url: str
    project_id: str
    email: str
    cookie: str
    directory: Path


@pytest.fixture
def server(tmp_path):
    port = free_port()
    database = tmp_path / "cocat.db"
    updates = tmp_path / "updates"
    updates.mkdir()
    email = "scientist@example.org"
    project_id = str(uuid.uuid4())
    run(
        "create-user",
        "--email",
        email,
        "--password",
        PASSWORD,
        "--db_path",
        str(database),
    )
    run(
        "create-room",
        "--owner",
        email,
        "--room-id",
        project_id,
        "--name",
        "bench",
        "--db_path",
        str(database),
    )
    process = subprocess.Popen(
        [
            str(BINARY),
            "serve",
            "--host",
            "127.0.0.1",
            "--port",
            str(port),
            "--update_dir",
            str(updates),
            "--db_path",
            str(database),
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        wait_for_port(port)
        url = f"http://127.0.0.1:{port}"
        yield Server(url, project_id, email, login(url, email), tmp_path)
    finally:
        process.terminate()
        process.wait(timeout=15)


def login(url: str, email: str) -> str:
    data = urllib.parse.urlencode({"username": email, "password": PASSWORD}).encode()
    request = urllib.request.Request(f"{url}/auth/jwt/login", data=data, method="POST")
    with urllib.request.urlopen(request) as response:
        header = response.headers["set-cookie"]
    return header.split("fastapiusersauth=")[1].split(";")[0]


@pytest.fixture
def cache(tmp_path):
    directory = tmp_path / "cache"
    directory.mkdir()
    return directory


@pytest.fixture
def project(server, cache):
    project = cc.Project.open(str(cache), server.project_id)
    project.connect(server.url, server.cookie)
    project.wait_synced(15)
    try:
        yield project
    finally:
        project.close()


@pytest.fixture
def other(server, tmp_path):
    """A second client in the same project, for changes made by someone else."""
    directory = tmp_path / "other"
    directory.mkdir()
    project = cc.Project.open(str(directory), server.project_id)
    project.connect(server.url, server.cookie)
    project.wait_synced(15)
    try:
        yield project
    finally:
        project.close()


@pytest.fixture
def colleague(server, tmp_path):
    """A second account, an editor of the project, connected from its own cache."""
    email = "colleague@example.org"
    database = str(server.directory / "cocat.db")
    run("create-user", "--email", email, "--password", PASSWORD, "--db_path", database)
    run(
        "add-user-to-room",
        "--email",
        email,
        "--room_id",
        server.project_id,
        "--db_path",
        database,
    )
    directory = tmp_path / "colleague"
    directory.mkdir()
    project = cc.Project.open(str(directory), server.project_id)
    project.connect(server.url, login(server.url, email))
    project.wait_synced(15)
    try:
        yield project
    finally:
        project.close()
