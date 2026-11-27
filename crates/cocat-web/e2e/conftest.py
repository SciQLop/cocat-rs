"""Browser tests of the SQL engine and its tools: a real server with a few public catalogues,
the web bundle (`trunk build` after `fetch-duckdb.sh`) and headless Chromium (Playwright).

Run: python -m pytest crates/cocat-web/e2e (needs cocat_rs, duckdb, playwright and its
chromium). Each test is small: the engine's memory limit is low and the data a few events.
"""

import os
import socket
import subprocess
import time
import urllib.parse
import urllib.request
from pathlib import Path

import pytest

playwright = pytest.importorskip("playwright.sync_api")
cc = pytest.importorskip("cocat_rs")

ROOT = Path(__file__).resolve().parents[3]
BINARY = ROOT / "target" / "debug" / "cocat-server"
DIST = ROOT / "crates" / "cocat-web" / "dist"
PASSWORD = "plum orbit lantern cello"
READER = "reader@example.org"
CURATOR = "curator@example.org"
# The text a catalogue's author could write to attack the page that shows it.
HOSTILE = '<img src=x onerror="window.pwned = 1">'

def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def cli(database, *arguments):
    subprocess.run([str(BINARY), *arguments, "--db_path", str(database)], check=True, capture_output=True)


def login(url, email):
    data = urllib.parse.urlencode({"username": email, "password": PASSWORD}).encode()
    with urllib.request.urlopen(urllib.request.Request(f"{url}/auth/jwt/login", data=data)) as answer:
        return answer.headers["set-cookie"].split("fastapiusersauth=")[1].split(";")[0]


def catalogue(url, curator, directory, name, folder, events):
    """A public catalogue holding `events`: (start, stop, attributes)."""
    public_id = cc.raw.create_public_catalogue(url, curator, name, folder=folder, source="Published lists")
    directory.mkdir()
    project = cc.Project.open(str(directory), public_id)
    project.connect(url, curator)
    project.wait_synced(15)
    made = [project.create_event(start, stop, attributes=attributes) for start, stop, attributes in events]
    project.create_catalogue(name=name, events=made)
    project.save(15)
    project.close()
    return public_id


def crossings(day, extra):
    return [(f"2020-01-{day:02d}T{hour:02d}:00:00", f"2020-01-{day:02d}T{hour:02d}:00:30",
             {"spacecraft": "C1", **extra}) for hour in range(0, 24, 2)]


def wait_for_snapshot(url, cookie, ids):
    """Until the server's snapshot holds every catalogue's attributes: a request starts its
    rewrite once they changed."""
    import json

    wanted = {f"attributes/{id}.arrow" for id in ids}
    for _ in range(300):
        request = urllib.request.Request(f"{url}/api/public/snapshot")
        request.add_header("cookie", f"fastapiusersauth={cookie}")
        with urllib.request.urlopen(request) as answer:
            if wanted <= set(json.load(answer)["files"]):
                return
        time.sleep(0.1)
    raise AssertionError("the snapshot never held the catalogues")


@pytest.fixture(scope="session")
def server(tmp_path_factory):
    if not (DIST / "sql-engine" / "vendor" / "duckdb-base.wasm").exists():
        pytest.skip("no web bundle with the SQL engine: run fetch-duckdb.sh, then trunk build")
    directory = tmp_path_factory.mktemp("server")
    database = directory / "cocat.db"
    (directory / "rooms").mkdir()
    cli(database, "create-user", "--email", READER, "--password", PASSWORD)
    cli(database, "create-user", "--email", CURATOR, "--password", PASSWORD)
    cli(database, "set-curator", "--email", CURATOR)
    port = free_port()
    environment = {**os.environ, "COCAT_WEB_DIR": str(DIST)}
    process = subprocess.Popen(
        [str(BINARY), "serve", "--host", "127.0.0.1", "--port", str(port),
         "--update_dir", str(directory / "rooms"), "--db_path", str(database)],
        env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    url = f"http://127.0.0.1:{port}"
    try:
        for _ in range(150):
            try:
                socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
                break
            except OSError:
                time.sleep(0.1)
        curator = login(url, CURATOR)
        ids = {
            "a": catalogue(url, curator, directory / "a", "Earth bow shock crossings, Cluster (A)",
                           "Literature/Earth", crossings(1, {"note": HOSTILE})),
            "b": catalogue(url, curator, directory / "b", "Earth bow shock crossings, Cluster (B)",
                           "Literature/Earth", crossings(1, {"quality": 1.0})),
            "icme": catalogue(url, curator, directory / "icme", "ICME multi-catalogue",
                              "Literature/Heliosphere",
                              [("2020-02-01T00:00:00", "2020-02-02T00:00:00", {"spacecraft": "Wind"})]),
        }
        reader = login(url, READER)
        wait_for_snapshot(url, reader, ids.values())
        yield {"url": url, "cookie": reader, "ids": ids}
    finally:
        process.terminate()
        process.wait(timeout=15)


@pytest.fixture(scope="session")
def browser():
    with playwright.sync_playwright() as driver:
        launched = driver.chromium.launch()
        yield launched
        launched.close()


@pytest.fixture
def page(browser, server):
    """The signed-in app page, with every request it makes recorded."""
    context = browser.new_context()
    context.add_cookies([{"name": "fastapiusersauth", "value": server["cookie"], "url": server["url"]}])
    opened = context.new_page()
    opened.requests = []
    opened.on("request", lambda request: opened.requests.append(request.url))
    opened.on("console", lambda message: print("console:", message.type, message.text[:300]))
    opened.on("pageerror", lambda error: print("pageerror:", str(error)[:300]))
    opened.goto(f"{server['url']}/ui/app")
    opened.wait_for_function("window.cocatSql !== undefined")
    yield opened
    context.close()


def sql(page, server, query, **options):
    """The engine's answer to `query`, as the page's tools would ask it."""
    options = {"snapshot": f"{server['url']}/api/public/snapshot", "maxRows": 50,
               "timeoutMs": 15000, "memoryLimit": "256MB", **options}
    return page.evaluate("([query, options]) => window.cocatSql.query(query, options)", [query, options])
