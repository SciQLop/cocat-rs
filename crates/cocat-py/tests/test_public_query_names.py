"""A search of the public catalogues that names one of them reads back as typed: its
`catalogue:` is a public catalogue's name, which the project itself does not know."""

import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

import cocat_rs as cc

BEPI = {
    "id": "public.amda.sharedcatalog_21",
    "name": "BepiCoordObs_Windows_of_Opportunities",
    "folder": "AMDA/BepiColombo",
    "source": "AMDA",
    "description": None,
    "role": "viewer",
    "changed": None,
    "count": 1174,
}

# As typed in SciQLop's search box, "Search in: Public catalogues".
TYPED = 'start:>=2020-01-01 catalogue:"BepiCoordObs_Windows_of_Opportunities" "Col 1":Parker NOT "Col 1":"Cone--Parker"'


@pytest.fixture
def server():
    """Lists one public catalogue; counts how often the list is asked."""
    asked = {"list": 0}

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            path = self.path.split("?")[0].rstrip("/")
            if path.endswith("/api/public"):
                asked["list"] += 1
                body = [BEPI]
            elif path.endswith("/api/public/events"):
                body = {"total": 0, "indexing": 0, "events": []}
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
    yield f"http://127.0.0.1:{http.server_port}", asked, http
    http.shutdown()


def opened(tmp_path, url):
    project = cc.Project.open(str(tmp_path), "5a5a5a5a-0000-4000-8000-0000000000ae")
    project.use_server(url, "cookie")
    return project


def test_a_public_search_naming_a_public_catalogue_reads_back_as_typed(tmp_path, server):
    url, _, _ = server
    project = opened(tmp_path, url)
    smart = project.create_smart_catalogue("Parker, not cone", TYPED, public=True)
    rule = smart.rule

    assert smart.query == TYPED
    project.close()

    again = opened(tmp_path, url)
    [reopened] = [c for c in again.catalogues() if c.name == "Parker, not cone"]
    assert reopened.query == TYPED, "read back as typed in a new session too"
    assert reopened.rule == rule, "the rule itself is untouched"
    again.close()


def test_without_the_server_the_search_still_reads_as_a_search(tmp_path, server):
    url, _, http = server
    project = opened(tmp_path, url)
    rule = project.create_smart_catalogue("Parker, not cone", TYPED, public=True).rule
    project.close()
    http.shutdown()
    http.server_close()

    offline = opened(tmp_path, url)
    [smart] = [c for c in offline.catalogues() if c.name == "Parker, not cone"]
    text = smart.query

    assert text is not None, "a search, not the rule's JSON"
    reread = offline.create_smart_catalogue("Again", text)
    assert reread.rule["where"] == rule["where"], f"{text!r} reads as the same rule"
    offline.close()
