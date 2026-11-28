"""Charts: Vega-Lite drawn in a sandboxed frame, in interpreter mode, from the query's rows
only (docs/design/browser-sql-analysis.md, section 4, rule 5). A spec cannot reach a URL,
the page or the window; the page shows the chart as an image, never as markup."""

import pytest
from conftest import HOSTILE, sql
from test_sql_tools import agent, call  # noqa: F401  (the fixture and the tool call)

BARS = {"mark": "bar", "encoding": {
    "x": {"field": "theme", "type": "nominal"}, "y": {"field": "n", "type": "quantitative"}}}
PER_THEME = "SELECT theme, count(*) AS n FROM clean_events GROUP BY theme ORDER BY theme"


def plot(page, server, spec, query=PER_THEME):
    answer = sql(page, server, query, maxRows=5000)
    return page.evaluate("([spec, answer]) => window.cocatSql.plot(spec, answer)", [spec, answer])


def left_the_page(page, server):
    return [url for url in page.requests if not url.startswith((server["url"], "blob:", "data:"))]


def test_a_bar_chart_is_drawn_from_the_rows(page, server):
    chart = plot(page, server, BARS)
    assert chart.get("error") is None, chart
    assert chart["svg"].startswith("<svg"), chart["svg"][:100]
    assert chart["svg"].count('aria-roledescription="bar"') == 2, "one bar per theme"
    assert chart["rowsPlotted"] == 2


@pytest.mark.parametrize("spec", [
    {**BARS, "data": {"url": "https://example.org/x.csv"}},
    {"layer": [BARS, {"data": {"url": "http://127.0.0.1:1/api/public"}, "mark": "point"}]},
    {"mark": "image", "encoding": {"url": {"value": "https://example.org/x.png"}}},
    {**BARS, "encoding": {**BARS["encoding"], "href": {"value": "https://example.org"}}},
    {**BARS, "usermeta": {"embedOptions": {"ast": False}}},
], ids=["data url", "layer data url", "image url", "href", "usermeta"])
def test_a_spec_that_names_a_url_is_refused_and_nothing_leaves(page, server, spec):
    chart = plot(page, server, spec)
    assert "its spec may not hold" in chart.get("error", ""), chart
    assert left_the_page(page, server) == []


@pytest.mark.parametrize("expression", [
    "window.location.href",
    "this.constructor",
    "datum.constructor.constructor('return 1')()",
    "parent.document.title",
    "fetch('https://example.org')",
])
def test_an_expression_reaches_no_window_and_runs_no_code(page, server, expression):
    spec = {**BARS, "transform": [{"calculate": expression, "as": "w"}]}
    chart = plot(page, server, spec)
    assert chart.get("error"), chart
    assert left_the_page(page, server) == []
    assert page.evaluate("window.pwned") is None


def test_rows_past_the_chart_cap_are_left_out_with_a_warning(page, server):
    chart = page.evaluate("([spec, answer]) => window.cocatSql.plot(spec, answer)", [
        {"mark": "point", "encoding": {"x": {"field": "i", "type": "quantitative"}}},
        {"columns": ["i"], "types": ["Int64"], "rows": [[i] for i in range(6000)], "truncated": False}])
    assert chart["rowsPlotted"] == 5000
    assert any("5000" in warning for warning in chart["warnings"]), chart


def test_the_plot_tool_shows_the_chart_as_an_image_beside_its_query(agent):  # noqa: F811
    answer = call(agent, "plot", {"sql": PER_THEME, "spec": BARS})
    assert answer == {"shown": True, "rows_plotted": 2, "warnings": []}, answer
    query = agent.locator("aside.query-panel .query").first
    assert PER_THEME in query.locator(".query-sql").inner_text()
    image = query.locator("img.query-chart")
    assert image.get_attribute("src").startswith("data:image/svg+xml,")
    assert agent.locator("aside.query-panel svg").count() == 0, "never inserted as markup"
    failed = call(agent, "plot", {"sql": PER_THEME, "spec": {**BARS, "data": {"url": "https://example.org"}}})
    assert "its spec may not hold \"url\"" in failed["error"], failed


def test_catalogue_text_in_a_chart_stays_text(agent, server):  # noqa: F811
    spec = {"mark": "bar", "encoding": {
        "x": {"field": "note", "type": "nominal"}, "y": {"aggregate": "count", "type": "quantitative"}}}
    answer = call(agent, "plot", {"sql": f"SELECT note FROM attributes('{server['ids']['a']}')", "spec": spec})
    assert answer["shown"] is True, answer
    assert agent.evaluate("window.pwned") is None
    assert agent.locator("aside.query-panel img:not(.query-chart)").count() == 0


def test_the_panel_offers_the_shown_result_as_csv(agent):  # noqa: F811
    call(agent, "run_sql", {"sql": "SELECT 'a,b' AS text, 2 AS n UNION ALL SELECT '=1+1', -3 ORDER BY n"})
    link = agent.locator("aside.query-panel .query").first.locator("a.query-csv")
    assert link.get_attribute("download").endswith(".csv")
    with agent.expect_download() as download:
        link.click()
    text = open(download.value.path(), encoding="utf-8", newline="").read()
    assert text == "text,n\r\n'=1+1,-3\r\n\"a,b\",2\r\n", repr(text)
