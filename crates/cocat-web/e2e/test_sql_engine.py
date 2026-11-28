"""The SQL engine in the browser: it answers queries on the snapshot, and it can reach nothing
else (docs/design/browser-sql-analysis.md, section 4, rules 1, 2 and 4)."""

import time

import pytest
from conftest import sql


def engine_frames(page):
    return [frame for frame in page.frames if frame.url.endswith("/sql-engine/engine.html")]


def engine_frame(page):
    return engine_frames(page)[0]


def test_queries_run_on_the_snapshot_and_its_survey_tables(page, server):
    answer = sql(page, server, "SELECT count(*) AS n FROM events")
    assert answer["rows"] == [[25]], answer
    assert answer["columns"] == ["n"]
    themes = sql(page, server, "SELECT DISTINCT theme FROM catalogue_tags ORDER BY theme")
    assert themes["rows"] == [["Bow shocks"], ["ICMEs, magnetic clouds, sheaths"]], themes
    assert answer["snapshot"]


def test_an_attribute_file_is_fetched_when_a_query_names_it_after_the_lock(page, server):
    a = server["ids"]["a"]
    answer = sql(page, server, f"SELECT spacecraft, count(*) AS n FROM attributes('{a}') GROUP BY ALL")
    assert answer.get("error") is None, answer
    assert answer["rows"] == [["C1", 12]]
    plain = sql(page, server, f"SELECT count(*) FROM read_parquet('attributes/{server['ids']['b']}.parquet')")
    assert plain["rows"] == [[12]], plain


@pytest.mark.parametrize("statement", [
    "SELECT * FROM read_parquet('https://example.org/x.parquet')",
    "SELECT * FROM read_csv('http://127.0.0.1:1/api/public')",
    "SELECT * FROM read_text('/etc/passwd')",
    "SELECT * FROM 'attributes/../../etc/passwd'",
    "ATTACH 'other.db' AS other",
    "COPY (SELECT 1) TO 'out.csv'",
    # INSTALL alone does nothing in DuckDB-wasm (it answers Success); loading is the step.
    "INSTALL httpfs; LOAD httpfs",
    "LOAD httpfs",
    "SET enable_external_access = true",
    "RESET enable_external_access",
    "SET lock_configuration = false",
    "SET memory_limit = '4GB'",
    "SET allowed_directories = ['/']",
])
def test_a_query_reaches_no_file_url_or_setting(page, server, statement):
    answer = sql(page, server, statement)
    assert answer.get("error"), f"{statement} ran: {answer}"
    assert answer["kind"] == "sql"
    # The frame's own scripts and wasm are blob: URLs the page made from bytes it fetched.
    elsewhere = [url for url in page.requests if not url.startswith((server["url"], "blob:"))]
    assert not elsewhere, elsewhere


def test_the_frame_has_no_cookie_no_network_and_no_way_to_the_page(page, server):
    sql(page, server, "SELECT 1")
    frame = engine_frame(page)
    cookie = frame.evaluate("() => { try { return document.cookie; } catch (error) { return 'refused'; } }")
    assert cookie in ("", "refused")
    fetched = frame.evaluate(
        "async () => { try { await fetch('/api/public'); return 'fetched'; } catch (error) { return 'refused'; } }")
    assert fetched == "refused"
    parent = frame.evaluate("() => { try { return parent.document.title; } catch (error) { return 'refused'; } }")
    assert parent == "refused"
    storage = frame.evaluate("() => { try { return localStorage.length; } catch (error) { return 'refused'; } }")
    assert storage == "refused"


def test_the_engine_ignores_messages_from_another_window(page, server):
    sql(page, server, "SELECT 1")
    page.evaluate("""() => {
        window.heard = [];
        addEventListener('message', (event) => window.heard.push(event.data));
        const other = document.createElement('iframe');
        other.setAttribute('sandbox', 'allow-scripts');
        other.srcdoc = `<script>for (let i = 0; i < parent.frames.length; i++)
            parent.frames[i].postMessage({type: 'query', id: 'foreign', sql: 'SELECT 42', maxRows: 1}, '*');<\\/script>`;
        document.body.append(other);
    }""")
    time.sleep(2)
    heard = page.evaluate("() => window.heard.filter((message) => message && message.id === 'foreign')")
    assert heard == []


def test_rows_are_capped(page, server):
    answer = sql(page, server, "SELECT * FROM range(10000)", maxRows=100)
    assert len(answer["rows"]) == 100
    assert answer["truncated"] is True
    whole = sql(page, server, "SELECT * FROM range(100)", maxRows=100)
    assert whole["truncated"] is False


def test_a_runaway_query_is_stopped_and_the_next_one_runs(page, server):
    started = time.monotonic()
    answer = sql(page, server, "SELECT count(*) FROM range(1000000000000)", timeoutMs=2000)
    assert answer["kind"] == "timeout", answer
    assert time.monotonic() - started < 15
    assert sql(page, server, "SELECT 42")["rows"] == [[42]]


def test_a_query_past_the_memory_limit_fails_clearly_and_the_page_lives(page, server):
    answer = sql(page, server, "SELECT length(list(i)) FROM range(100000000) t(i)")
    assert answer.get("error"), answer
    assert answer["kind"] in ("memory", "sql"), answer
    assert "memory" in answer["error"].lower(), answer
    assert page.evaluate("1 + 1") == 2
    assert sql(page, server, "SELECT 42")["rows"] == [[42]]


def test_a_swapped_engine_file_is_refused(page, server):
    # The Parquet extension runs inside DuckDB: the page checks its hash before handing it
    # over, so a server (or a cache) that serves other bytes starts no engine.
    page.route("**/sql-engine/vendor/parquet.duckdb_extension.wasm",
               lambda route: route.fulfill(body=b"\0asm\1\0\0\0 other bytes"))
    answer = sql(page, server, "SELECT 42")
    assert answer["kind"] == "start", answer
    assert "parquet.duckdb_extension.wasm is not the pinned file" in answer["error"], answer
    assert engine_frames(page) == []


def test_the_engine_is_dropped_after_a_query_that_grew_it_and_the_next_one_starts_anew(page, server):
    grown = sql(page, server, "SELECT count(*) FROM events", recycleMB=1)
    assert grown["rows"] == [[25]], grown
    assert grown["heapBytes"] > 2**20
    page.wait_for_function("() => !document.querySelector('iframe[title=\"SQL engine\"]')")
    assert sql(page, server, "SELECT 42", recycleMB=4096)["rows"] == [[42]]
    assert len(engine_frames(page)) == 1


def test_an_idle_engine_is_dropped(page, server):
    assert sql(page, server, "SELECT 42", idleMs=500)["rows"] == [[42]]
    page.wait_for_function("() => !document.querySelector('iframe[title=\"SQL engine\"]')", timeout=5000)
    assert sql(page, server, "SELECT 43")["rows"] == [[43]]
