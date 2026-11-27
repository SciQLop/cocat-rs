"""The page's SQL tools, driven as a browser AI drives them: through document.modelContext,
where the page registers them. Headless Chromium has no WebMCP, so a stand-in modelContext
keeps what the page registers; the tools themselves are the page's own."""

import json

import pytest
from conftest import HOSTILE

MODEL_CONTEXT = """
document.modelContext = {
  tools: {},
  registerTool(tool) { this.tools[tool.name] = tool; return Promise.resolve(); },
};
"""


@pytest.fixture
def agent(browser, server):
    context = browser.new_context()
    context.add_cookies([{"name": "fastapiusersauth", "value": server["cookie"], "url": server["url"]}])
    context.add_init_script(MODEL_CONTEXT)
    page = context.new_page()
    page.on("console", lambda message: print("console:", message.type, message.text[:300]))
    page.goto(f"{server['url']}/ui/app")
    page.wait_for_function("Object.keys(document.modelContext.tools).length >= 4")
    yield page
    context.close()


def call(page, name, arguments):
    text = page.evaluate(
        "([name, input]) => document.modelContext.tools[name].execute(input)", [name, arguments])
    return json.loads(text)


def test_the_page_registers_the_sql_tools_read_only(agent):
    tools = agent.evaluate("""() => Object.values(document.modelContext.tools).map(
        (tool) => [tool.name, tool.annotations.readOnlyHint, tool.annotations.untrustedContentHint || false])""")
    assert sorted(tools) == [
        ["describe_data", True, True],
        ["open_public_catalogue", True, False],
        ["run_sql", True, True],
        ["search_public_catalogues", True, True],
    ]


def test_describe_data_gives_the_guide_the_names_and_a_catalogues_attributes(agent, server):
    described = call(agent, "describe_data", {})
    assert "ALWAYS narrow" in described["guide"]
    pairs = {(row["theme"], row["observer"]) for row in described["phenomena_and_observers"]}
    assert ("Bow shocks", "Earth (magnetosphere)") in pairs
    with_attributes = call(agent, "describe_data", {"catalogue": server["ids"]["a"]})
    columns = [column["column"] for column in with_attributes["attributes"]]
    assert "note" in columns and "spacecraft" in columns


def test_run_sql_answers_rows_and_the_page_shows_the_query_and_its_result(agent):
    answer = call(agent, "run_sql", {"sql": "SELECT theme, count(*) AS n FROM clean_events GROUP BY theme ORDER BY theme"})
    assert answer["rows"] == [["Bow shocks", 24], ["ICMEs, magnetic clouds, sheaths", 1]], answer
    assert answer["snapshot"]
    panel = agent.locator("aside.query-panel")
    assert "SELECT theme, count(*) AS n FROM clean_events" in panel.locator(".query-sql").first.inner_text()
    cells = panel.locator(".query").first.locator("td").all_inner_texts()
    assert cells == ["Bow shocks", "24", "ICMEs, magnetic clouds, sheaths", "1"]


def test_a_pairwise_query_on_everything_is_steered_before_it_runs(agent):
    answer = call(agent, "run_sql", {"sql": "SELECT * FROM agreement()"})
    assert "one phenomenon" in answer["error"], answer
    narrowed = call(agent, "run_sql", {"sql": "SELECT label_a, label_b, compared, matched FROM agreement(phenomenon := 'Bow shocks')"})
    assert len(narrowed["rows"]) == 2, narrowed
    assert {row[3] for row in narrowed["rows"]} == {1.0}
    shown = agent.locator("aside.query-panel .query-error").first.inner_text()
    assert "one phenomenon" in shown


def test_a_query_past_the_memory_limit_answers_clearly_and_the_next_one_runs(agent):
    answer = call(agent, "run_sql", {"sql": "SELECT length(list(i)) FROM range(200000000) t(i)"})
    error = answer.get("error", "")
    assert "more memory than the browser allows" in error or "longer than" in error, answer
    assert agent.evaluate("1 + 1") == 2
    assert call(agent, "run_sql", {"sql": "SELECT 42 AS n"})["rows"] == [[42]]


def test_catalogue_text_is_shown_as_text_never_as_markup(agent, server):
    answer = call(agent, "run_sql", {"sql": f"SELECT note FROM attributes('{server['ids']['a']}') LIMIT 1"})
    assert answer["rows"] == [[HOSTILE]], answer
    panel = agent.locator("aside.query-panel")
    assert panel.locator("td").first.inner_text() == HOSTILE
    assert panel.locator("img").count() == 0
    assert agent.evaluate("window.pwned") is None
