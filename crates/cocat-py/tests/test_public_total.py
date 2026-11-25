"""A search of the public catalogues counts what the server found, not the 10,000 events at
most it sends: a server stand-in answers a total larger than its events."""

import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

import cocat_rs as cc

EVENTS = [
    {"catalogue": "public.shocks", "event": {
        "uuid": f"5a5a5a5a-0000-4000-8000-00000000000{n}",
        "start": f"2020-01-0{n}T00:00:00Z", "stop": f"2020-01-0{n}T01:00:00Z",
    }}
    for n in (1, 2)
]


@pytest.fixture
def server():
    """Answers `/api/public` with no catalogues and `/api/public/events` with the two
    events and `answer["total"]`."""
    answer = {"total": 25_000}

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            path = self.path.split("?")[0].rstrip("/")
            if path.endswith("/api/public"):
                body = []
            elif path.endswith("/api/public/events"):
                body = {"total": answer["total"], "indexing": 0, "events": EVENTS}
            else:
                self.send_error(404)
                return
            data = json.dumps(body).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def log_message(self, *_):
            pass

    http = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=http.serve_forever, daemon=True).start()
    yield f"http://127.0.0.1:{http.server_port}", answer
    http.shutdown()


@pytest.fixture
def project(tmp_path, server):
    url, _ = server
    project = cc.Project.open(str(tmp_path), "5a5a5a5a-0000-4000-8000-0000000000ad")
    project.use_server(url, "cookie")
    yield project
    project.close()


def test_a_preview_counts_every_event_the_server_found(project):
    found = project.preview("start:>1900", public=True, limit=50)
    assert len(found["uuid"]) == 2
    assert found["count"] == 25_000
    assert found["capped"] is True


def test_a_search_says_its_total_and_that_it_was_capped(project):
    found = project.search_public("start:>1900")
    assert len(found["uuid"]) == 2
    assert (found["total"], found["capped"]) == (25_000, True)


def test_a_smart_catalogue_says_it_shows_only_the_first_events(project):
    smart = project.create_smart_catalogue("Old", "start:>1900", public=True)
    with pytest.warns(UserWarning, match="only the first 2 of 25,000 events"):
        assert len(smart.event_arrays()["uuid"]) == 2


def test_an_answer_that_holds_everything_is_not_capped(tmp_path, server):
    url, answer = server
    answer["total"] = 2
    project = cc.Project.open(str(tmp_path), "5a5a5a5a-0000-4000-8000-0000000000ae")
    project.use_server(url, "cookie")
    found = project.preview("start:>1900", public=True)
    assert (found["count"], found["capped"]) == (2, False)
    assert project.search_public("start:>1900")["capped"] is False
    assert project.preview("start:>1900")["capped"] is False, "nothing public asked"
    project.close()


def test_a_search_of_the_public_catalogues_reads_back_as_typed(project):
    smart = project.create_smart_catalogue("Old", "start:>2018", public=True)
    assert (smart.searches_public, smart.query) == (True, "start:>2018")
    smart.set_query("start:<2020", public=True)
    assert smart.query == "start:<2020"
