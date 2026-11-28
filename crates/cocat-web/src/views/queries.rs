//! Every query the page's SQL tools ran for an AI agent, with its raw result, so the visitor
//! sees what the agent's numbers come from (docs/design/browser-sql-analysis.md, section 3).
//! The page shows them, not the agent: an agent cannot hide or reword a query. Cells are text:
//! catalogue text is written by third parties.

use leptos::prelude::*;
use serde_json::Value;

use crate::sql_tools::{cell_text, csv, data_url};

/// A query a tool ran, or would have run, and what the engine answered.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryRecord {
    pub tool: &'static str,
    pub sql: String,
    pub result: Value,
}

#[derive(Debug, Clone, Copy)]
pub struct QueryLog(RwSignal<Vec<QueryRecord>>);

impl QueryLog {
    pub fn new() -> Self {
        Self(RwSignal::new(Vec::new()))
    }

    pub fn record(self, tool: &'static str, sql: &str, result: &Value) {
        let sql = sql.to_owned();
        let result = result.clone();
        self.0
            .update(|queries| queries.push(QueryRecord { tool, sql, result }));
    }
}

impl Default for QueryLog {
    fn default() -> Self {
        Self::new()
    }
}

#[component]
pub fn QueryPanel(log: QueryLog) -> impl IntoView {
    let open = RwSignal::new(true);
    let count = move || log.0.with(Vec::len);
    move || {
        (count() > 0).then(|| {
            view! {
                <aside class="query-panel" aria-label="Queries the assistant ran">
                    <header>
                        <button type="button" class="link" aria-expanded=move || open.get().to_string()
                            on:click=move |_| open.update(|open| *open = !*open)>
                            {move || format!("Queries the assistant ran ({})", count())}
                        </button>
                        <button type="button" class="icon" aria-label="Clear the queries"
                            on:click=move |_| log.0.set(Vec::new())>"✕"</button>
                    </header>
                    <Show when=move || open.get()>
                        <ol class="queries">
                            {move || log.0.get().into_iter().enumerate().rev().map(|(at, query)| view! { <Query query number=at + 1/> }).collect_view()}
                        </ol>
                    </Show>
                </aside>
            }
        })
    }
}

#[component]
fn Query(query: QueryRecord, number: usize) -> impl IntoView {
    let result = query.result;
    let summary = match result.get("error").and_then(Value::as_str) {
        Some(_) => "failed".to_owned(),
        None => {
            let rows = result["rows"].as_array().map_or(0, Vec::len);
            let more = if result["truncated"].as_bool().unwrap_or(false) {
                "+"
            } else {
                ""
            };
            let noun = if rows == 1 && more.is_empty() {
                "row"
            } else {
                "rows"
            };
            format!("{rows}{more} {noun}")
        }
    };
    let snapshot = result["snapshot"]
        .as_str()
        .map(|version| format!("data {version}"));
    view! {
        <li class="query">
            <p class="query-meta">{query.tool}" · "{summary}{snapshot.map(|text| format!(" · {text}"))}</p>
            <pre class="query-sql">{query.sql}</pre>
            {match result.get("error").and_then(Value::as_str) {
                Some(error) => view! { <p class="query-error" role="alert">{error.to_owned()}</p> }.into_any(),
                None => view! { <Chart result=result.clone()/> <ResultTable result number/> }.into_any(),
            }}
        </li>
    }
}

/// The chart `plot` drew, as an image: SVG in an `img` runs no script and loads nothing.
#[component]
fn Chart(result: Value) -> impl IntoView {
    let chart = result["chart"]
        .as_str()
        .map(|svg| data_url("image/svg+xml", svg));
    let refused = result["chart_error"].as_str().map(str::to_owned);
    view! {
        {chart.map(|src| view! { <img class="query-chart" src=src alt="Chart drawn from this query's rows"/> })}
        {refused.map(|error| view! { <p class="query-error" role="alert">"No chart: "{error}</p> })}
    }
}

#[component]
fn ResultTable(result: Value, number: usize) -> impl IntoView {
    let download = data_url("text/csv;charset=utf-8", &csv(&result));
    let columns: Vec<String> = result["columns"]
        .as_array()
        .map(|columns| columns.iter().map(cell_text).collect())
        .unwrap_or_default();
    let rows: Vec<Vec<String>> = result["rows"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    row.as_array()
                        .map(|cells| cells.iter().map(cell_text).collect())
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default();
    view! {
        <a class="link query-csv" href=download download=format!("query-{number}.csv")>"Download CSV"</a>
        <div class="query-result">
            <table>
                <thead><tr>{columns.into_iter().map(|name| view! { <th>{name}</th> }).collect_view()}</tr></thead>
                <tbody>
                    {rows.into_iter().map(|row| view! {
                        <tr>{row.into_iter().map(|cell| {
                            let title = cell.clone();
                            view! { <td title=title>{cell}</td> }
                        }).collect_view()}</tr>
                    }).collect_view()}
                </tbody>
            </table>
        </div>
    }
}
