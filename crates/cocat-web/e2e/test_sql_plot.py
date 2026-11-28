"""Charts: Vega-Lite drawn in a sandboxed frame, in interpreter mode, from the query's rows
only (docs/design/browser-sql-analysis.md, section 4, rule 5). A spec cannot reach a URL,
the page or the window; the page shows the chart as an image, never as markup."""

import time

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


def plot_watching_frames(page, spec, answer, **options):
    """plot's answer, and how many chart frames it opened and left in the page."""
    return page.evaluate("""async ([spec, answer, options]) => {
        let opened = 0;
        const watch = new MutationObserver((changes) => changes.forEach((change) =>
            change.addedNodes.forEach((node) => { if (node.title === "Chart") opened++; })));
        watch.observe(document.body, { childList: true });
        const chart = await window.cocatSql.plot(spec, answer, options);
        await new Promise((done) => setTimeout(done, 50));
        watch.disconnect();
        return [chart, opened];
    }""", [spec, answer, options])


def chart_frames(page):
    return page.locator('iframe[title="Chart"]').count()


NUMBERS = {"columns": ["i"], "types": ["Int64"], "rows": [[i] for i in range(5000)], "truncated": False}
POINTS = {"mark": "point", "encoding": {"x": {"field": "i", "type": "quantitative"}}}


def test_a_chart_past_its_time_is_stopped_and_its_frame_removed(page, server):
    """The limit runs out while the chart still loads (Vega-Lite held back 1.5 s, then a 5,000-cell facet): plot
    answers, and no chart frame is left, nor opens later, to go on with the chart.
    A frame that computes without pause holds the page's thread too, so its limit can only
    act between Vega's steps: the checks before any frame starts are what bound that."""
    def held_back(route):
        time.sleep(1.5)
        route.continue_()
    page.route("**/vendor/vega-lite.min.js", held_back)
    slow = {"facet": {"field": "i", "type": "nominal"}, "columns": 50, "spec": POINTS}
    chart, left = page.evaluate("""async ([spec, answer]) => {
        const chart = await window.cocatSql.plot(spec, answer, { timeoutMs: 300 });
        let left = document.querySelectorAll('iframe[title="Chart"]').length;
        const watch = new MutationObserver((changes) => changes.forEach((change) =>
            change.addedNodes.forEach((node) => { if (node.title === "Chart") left++; })));
        watch.observe(document.body, { childList: true });
        await new Promise((done) => setTimeout(done, 3000));
        watch.disconnect();
        return [chart, left];
    }""", [slow, NUMBERS])
    assert "no answer in 0.3 s" in chart.get("error", ""), chart
    assert left == 0, "the frame is thrown away with the chart"


def test_the_frame_is_removed_after_every_chart(page, server):
    chart, opened = plot_watching_frames(page, POINTS, NUMBERS)
    assert chart.get("error") is None, chart
    assert opened == 1
    assert chart_frames(page) == 0


@pytest.mark.parametrize("spec", [
    {**POINTS, "data": {"sequence": {"start": 0, "stop": 1e9, "as": "i"}}},
    {**POINTS, "data": {"values": [{"i": 1}]}},
    {"layer": [POINTS, {**POINTS, "data": {"sequence": {"start": 0, "stop": 1e9}}}]},
    {"hconcat": [POINTS, {**POINTS, "data": {"values": [{"i": 1}]}}]},
    {"facet": {"field": "i"}, "spec": {**POINTS, "data": {"sequence": {"start": 0, "stop": 1e9}}}},
    {**POINTS, "datasets": {"big": [{"i": 1}]}},
    {**POINTS, "transform": [{"lookup": "i", "from": {"data": {"values": [{"i": 1}]}, "key": "i"}}]},
    {**POINTS, "transform": [{"calculate": "sequence(0, 1e9)", "as": "s"}]},
    {**POINTS, "transform": [{"calculate": "pad('', 1e9)", "as": "s"}]},
    {**POINTS, "transform": [{"flatten": ["i"]}]},
    {**POINTS, "transform": [{"fold": [f"f{n}" for n in range(1000)]}]},
    {**POINTS, "transform": [{"impute": "i", "key": "i", "keyvals": {"start": 0, "stop": 1e9}}]},
    {**POINTS, "transform": [{"quantile": "i", "step": 1e-9}]},
    {**POINTS, "transform": [{"density": "i", "steps": 1e9}]},
    {**POINTS, "encoding": {"x": {"field": "i", "bin": {"step": 1e-9}, "type": "quantitative"}}},
    {**POINTS, "encoding": {"x": {"field": "i", "bin": {"maxbins": 1e9}, "type": "quantitative"}}},
    {**POINTS, "encoding": {"x": {"field": "i", "type": "quantitative", "axis": {"tickCount": 1e9}}}},
    {**POINTS, "encoding": {"x": {"field": "i", "type": "quantitative", "scale": {"nice": 1e9}}}},
    {**POINTS, "width": 1e9},
    {**POINTS, "config": {"view": {"continuousHeight": 1e9}}},
], ids=["sequence", "values", "layer data", "concat data", "facet data", "datasets", "lookup data",
        "sequence()", "pad()", "flatten", "fold", "impute keyvals", "quantile step", "density steps",
        "bin step", "maxbins", "tickCount", "nice", "width", "view size"])
def test_a_spec_that_makes_rows_or_work_of_its_own_is_refused_before_any_frame(page, server, spec):
    chart, opened = plot_watching_frames(page, spec, NUMBERS)
    assert chart.get("error"), chart
    assert opened == 0, "refused before a frame starts"


@pytest.mark.parametrize("spec", [
    {**POINTS, "encoding": {"x": {"field": "i", "bin": {"maxbins": 50}, "type": "quantitative"}, "y": {"aggregate": "count"}}},
    {**POINTS, "transform": [{"density": "i"}], "encoding": {"x": {"field": "value", "type": "quantitative"}}},
    {**POINTS, "transform": [{"fold": ["i"]}]},
    {**POINTS, "width": 800, "height": 300},
], ids=["maxbins", "density", "small fold", "size"])
def test_ordinary_charts_still_draw(page, server, spec):
    chart, _ = plot_watching_frames(page, spec, NUMBERS)
    assert chart.get("error") is None, chart


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
    assert "its spec may not hold \"data\"" in failed["error"], failed


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
