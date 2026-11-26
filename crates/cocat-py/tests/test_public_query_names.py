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

# More public catalogues, with names a search must quote; one shares a project catalogue's name.
ODD = [
    dict(BEPI, id="public.donki.cme", name="DONKI CMEs", count=10),
    dict(BEPI, id="public.odd.quote", name='Say "hi"', count=1),
    dict(BEPI, id="public.odd.unicode", name="Événements solaires", count=1),
    dict(BEPI, id="public.odd.same", name="Same name", count=1),
]

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
                body = [BEPI, *ODD] if asked.get("odd") else [BEPI]
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


# SciQLop, "Search in: Public catalogues", 2026-11-26: `catalogue:` offered the project's own
# catalogues, which the public search then refused ("No catalogue is named “jet candidates”").


def test_in_the_public_scope_completion_offers_the_public_catalogues(tmp_path, server):
    url, _, _ = server
    project = opened(tmp_path, url)
    project.create_catalogue(name="jet candidates")

    public = project.vocabulary(public=True)
    found = cc.complete("start:>2024 catalogue:", 22, public)

    assert public["catalogues"] == [BEPI["name"]]
    assert [s["insert"] for s in found["suggestions"]] == [f"catalogue:{BEPI['name']} "]
    assert project.vocabulary()["catalogues"] == ["jet candidates"], "the project scope is unchanged"
    project.close()


def test_in_the_public_scope_the_text_reads_with_the_public_names(tmp_path, server):
    url, _, _ = server
    project = opened(tmp_path, url)
    project.create_catalogue(name="jet candidates")

    assert project.query_kind(f'start:>2024 catalogue:"{BEPI["name"]}"', public=True) == "rule"
    assert project.query_kind('start:>2024 catalogue:"jet candidates"', public=True) == "broken"
    assert project.query_kind('start:>2024 catalogue:"jet candidates"') == "rule", "project scope"
    assert project.query_kind(f'catalogue:"{BEPI["name"]}"') == "broken", "project scope"
    project.close()


@pytest.mark.parametrize("scope", ["this catalogue", "all events", "public catalogues"])
def test_every_name_completion_offers_reads_in_its_scope(tmp_path, server, scope):
    url, asked, _ = server
    asked["odd"] = True
    project = opened(tmp_path, url)
    for name in ["jet candidates", 'Say "hi"', "Événements", "Same name"]:
        project.create_catalogue(name=name)
    source = project.create_catalogue(name="Source") if scope == "this catalogue" else None
    public = scope == "public catalogues"

    found = cc.complete("start:>2024 catalogue:", 22, project.vocabulary(public=public))
    offered = [s["insert"].strip() for s in found["suggestions"]]

    assert len(offered) == (len(ODD) + 1 if public else len(project.catalogues())), offered
    for term in offered:
        assert project.query_kind(term, public=public) == "rule", f"{term} offered, checked broken"
        project.parse_query(term, source=source, public=public)  # what the search reads
    project.close()
