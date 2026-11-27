"""A search of the public catalogues that names one of them reads back as typed: its
`catalogue:` is a public catalogue's name, which the project itself does not know."""

import json
import threading
import time
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
    dict(BEPI, id="public.other.cme", name="DONKI CMEs", count=3),
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
                if asked.get("refuse"):
                    self.send_error(503)
                    return
                time.sleep(asked.get("slow", 0))
                body = [BEPI, *ODD] if asked.get("odd") else [BEPI]
                if asked.get("attributes"):
                    body = [dict(entry, attributes=["Col 1", "speed"]) for entry in body]
                if asked.get("values"):
                    body = [
                        dict(BEPI, attributes=["region_name"], attribute_values={"region_name": ["Magnetosheath", "Magnetopause Transition"]}),
                        dict(BEPI, id="public.grmb.c2", name="C2 regions", attributes=["region_name"], attribute_values={"region_name": ["Plasmasheet", "Magnetosheath"]}),
                    ]
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
    for name in ["jet candidates", 'Say "hi"', "Événements", "Same name", "Twice", "Twice"]:
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


# A server counting its public catalogues after a start lists them in more than 5 s: the check
# then read no names at all and called every public catalogue a mistake.
def test_a_slow_list_of_public_catalogues_is_waited_for(tmp_path, server):
    url, asked, _ = server
    asked["slow"] = 6
    project = opened(tmp_path, url)

    assert project.query_kind(f'catalogue:"{BEPI["name"]}"', public=True) == "rule"
    project.close()


def test_without_a_list_of_public_catalogues_no_name_is_called_a_mistake(tmp_path, server):
    url, asked, _ = server
    asked["refuse"] = True
    project = opened(tmp_path, url)
    typed = f'catalogue:"{BEPI["name"]}"'

    with pytest.raises(RuntimeError, match="public catalogues"):
        project.query_kind(typed, public=True)
    with pytest.raises(RuntimeError, match="public catalogues"):
        project.preview(typed, public=True)
    assert project.vocabulary(public=True)["catalogues"] == [], "nothing to offer"
    project.close()


# Vibe, 2026-11-26: a project catalogue named like the public one a search names made the
# getter read both lists at once, find the name twice, and give no text.
def test_a_public_search_reads_back_beside_a_project_catalogue_of_the_same_name(tmp_path, server):
    url, _, _ = server
    project = opened(tmp_path, url)
    project.create_catalogue(name=BEPI["name"])
    smart = project.create_smart_catalogue("Bepi shocks", TYPED, public=True)

    assert smart.query == TYPED
    project.close()


# Vibe, 2026-11-26: the server's refusal came back as a ServerError, which `except
# cc.QueryError` does not catch, and with no position to mark.
def test_a_public_search_with_a_mistake_raises_a_query_error(tmp_path, server):
    url, _, _ = server
    project = opened(tmp_path, url)

    with pytest.raises(cc.QueryError) as refused:
        project.search_public("tag:shock AND")
    assert refused.value.position == 13
    with pytest.raises(cc.QueryError):
        project.search_public('catalogue:"Nope"')
    project.close()


# Vibe, 2026-11-26: in a search of the public catalogues `auhtor:alice` found nothing and
# said nothing. A server that lists their attribute keys has them read as the project's are.
def test_a_public_search_refuses_a_name_no_public_event_has(tmp_path, server):
    url, asked, _ = server
    asked["attributes"] = True
    project = opened(tmp_path, url)

    assert project.query_kind("auhtor:alice", public=True) == "broken"
    assert project.query_kind('"Col 1":Parker speed:>1', public=True) == "rule"
    for refused in (
        lambda: project.parse_query("auhtor:alice", public=True),
        lambda: project.preview("auhtor:alice", public=True),
        lambda: project.search_public("auhtor:alice"),
    ):
        with pytest.raises(cc.QueryError, match="author"):
            refused()
    assert project.vocabulary(public=True)["attributes"] == ["Col 1", "speed"]
    project.close()


def test_a_server_that_lists_no_attribute_keys_refuses_no_name(tmp_path, server):
    url, _, _ = server
    project = opened(tmp_path, url)

    assert project.query_kind("auhtor:alice", public=True) == "rule"
    project.close()


# SciQLop, "Search in: Public catalogues", 2026-11-27: `region_name:Ma` offered nothing, though
# the public events shown carried "Magnetosheath" and "Magnetopause Transition".


def test_in_the_public_scope_completion_offers_the_public_attribute_values(tmp_path, server):
    url, asked, _ = server
    asked["values"] = True
    project = opened(tmp_path, url)

    vocabulary = project.vocabulary(public=True)
    assert vocabulary["attribute_values"]["region_name"] == [
        "Magnetosheath",
        "Magnetopause Transition",
        "Plasmasheet",
    ], "the catalogues' values merged, each once, in the order listed"
    text = "start:>2023 AND region_name:Ma"
    offered = [s["insert"] for s in cc.complete(text, len(text), vocabulary)["suggestions"]]
    assert offered == ["region_name:Magnetosheath ", 'region_name:"Magnetopause Transition" ']
    project.close()
