//! Registers the page's tools with an AI agent in the browser, when the browser offers WebMCP
//! (<https://webmachinelearning.github.io/webmcp/>). What the tools answer is in
//! [`crate::agent_tools`]; this only bridges to JavaScript.
//!
//! The API is `document.modelContext`; early Chrome builds, Brave and the `@mcp-b/global`
//! polyfill put it on `navigator`, so both are tried. Without either, nothing happens.

use js_sys::{Function, JSON, Object, Promise, Reflect};
use serde_json::Value;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

use crate::agent_tools::{self, OPEN, SEARCH};
use crate::api;
use crate::projects::api_url;
use crate::public::PublicCatalogue;
use crate::sql_tools::{
    self, DESCRIBE, IDLE_MS, MEMORY_LIMIT, PLOT, PLOT_ROWS, RECYCLE_MB, RUN_SQL, TIMEOUT_MS,
    VOCABULARY_SQL,
};
use crate::views::projects::Projects;
use crate::views::queries::QueryLog;

pub fn offer_tools(projects: Projects, log: QueryLog) {
    let Some(context) = model_context() else {
        return;
    };
    let Ok(register) = Reflect::get(&context, &"registerTool".into()) else {
        return;
    };
    let Some(register) = register.dyn_ref::<Function>() else {
        return;
    };
    for definition in agent_tools::definitions()
        .into_iter()
        .chain(sql_tools::definitions())
    {
        let Some(name) = definition["name"].as_str().map(str::to_owned) else {
            continue;
        };
        let Some(tool) = to_js(&definition) else {
            continue;
        };
        let tool_name = name.clone();
        let execute = Closure::<dyn Fn(JsValue) -> Promise>::new(move |input: JsValue| {
            let (input, name) = (from_js(&input), name.clone());
            future_to_promise(async move {
                // An answer the agent reads says what went wrong; a rejection would not.
                let answer = run(projects, log, &name, &input)
                    .await
                    .unwrap_or_else(|error| serde_json::json!({ "error": error }));
                // A string of JSON: Brave's Leo reads a tool's result as text, and the spec
                // serializes whatever it gets, so text works for both.
                Ok(JsValue::from_str(&answer.to_string()))
            })
        });
        let _ = Reflect::set(&tool, &"execute".into(), execute.as_ref());
        // The tools live as long as the page: nothing to unregister them for.
        execute.forget();
        match register.call1(&context, &tool) {
            Err(error) => refused(&tool_name, &error),
            Ok(answer) => {
                if let Ok(registering) = answer.dyn_into::<Promise>() {
                    let on_refusal =
                        Closure::once(move |error: JsValue| refused(&tool_name, &error));
                    let _ = registering.catch(&on_refusal);
                    on_refusal.forget();
                }
            }
        }
    }
}

/// Says in the console why the browser would not take a tool, which it otherwise keeps to
/// itself.
fn refused(name: &str, error: &JsValue) {
    let message = format!("cocat: the browser refused the WebMCP tool {name}:");
    web_sys::console::warn_2(&message.into(), error);
}

async fn run(
    projects: Projects,
    log: QueryLog,
    name: &str,
    input: &Value,
) -> Result<Value, String> {
    match name {
        SEARCH => agent_tools::search(&public_catalogues(projects).await?, input),
        OPEN => {
            let all = public_catalogues(projects).await?;
            let catalogue = agent_tools::chosen(&all, input)?;
            projects.switch(&catalogue.project());
            Ok(serde_json::json!({"opened": catalogue.id, "name": catalogue.name}))
        }
        RUN_SQL => {
            let (sql, rows) = sql_tools::sql_input(input).inspect_err(|why| {
                let sql = input.get("sql").and_then(Value::as_str).unwrap_or_default();
                log.record(RUN_SQL, sql, &serde_json::json!({ "error": why }));
            })?;
            let raw = query(projects, &sql, rows).await;
            log.record(RUN_SQL, &sql, &raw);
            Ok(sql_tools::answer(&raw))
        }
        PLOT => {
            let (sql, spec) = sql_tools::plot_input(input).inspect_err(|why| {
                let sql = input.get("sql").and_then(Value::as_str).unwrap_or_default();
                log.record(PLOT, sql, &serde_json::json!({ "error": why }));
            })?;
            let mut raw = query(projects, &sql, PLOT_ROWS).await;
            let drawn = match raw.get("error") {
                Some(_) => Value::Null,
                None => bridge_call("plot", &[&spec, &raw]).await,
            };
            if let Some(map) = raw.as_object_mut() {
                match (drawn.get("svg"), drawn.get("error")) {
                    (Some(svg), _) => map.insert("chart".into(), svg.clone()),
                    (_, Some(error)) => map.insert("chart_error".into(), error.clone()),
                    _ => None,
                };
            }
            log.record(PLOT, &sql, &raw);
            Ok(sql_tools::plot_answer(&raw, &drawn))
        }
        DESCRIBE => {
            let attributes_sql = sql_tools::catalogue_sql(input)?;
            let vocabulary = query(projects, VOCABULARY_SQL, 500).await;
            log.record(DESCRIBE, VOCABULARY_SQL, &vocabulary);
            let attributes = match attributes_sql {
                Some(sql) => {
                    let found = query(projects, &sql, 3).await;
                    log.record(DESCRIBE, &sql, &found);
                    Some(found)
                }
                None => None,
            };
            Ok(sql_tools::description(&vocabulary, attributes.as_ref()))
        }
        _ => Err(format!("no tool {name}")),
    }
}

async fn public_catalogues(projects: Projects) -> Result<Vec<PublicCatalogue>, String> {
    api::read::<Vec<PublicCatalogue>>(&projects.api(), &["public"])
        .await
        .map_err(|error| error.to_string())
}

/// The engine's answer to `sql`, through the page's side of it (`sql-engine/bridge.js`):
/// rows, or an error and its kind.
async fn query(projects: Projects, sql: &str, max_rows: u64) -> Value {
    let Some(snapshot) = api_url(&projects.api(), &["public", "snapshot"]) else {
        return serde_json::json!({ "error": "no snapshot address", "kind": "start" });
    };
    let options = serde_json::json!({
        "snapshot": snapshot,
        "maxRows": max_rows,
        "timeoutMs": TIMEOUT_MS,
        "memoryLimit": MEMORY_LIMIT,
        "recycleMB": RECYCLE_MB,
        "idleMs": IDLE_MS,
    });
    bridge_call("query", &[&Value::from(sql), &options]).await
}

/// `window.cocatSql[method](...arguments)`, awaited: its answer, or `{error, kind: "start"}`.
async fn bridge_call(method: &str, arguments: &[&Value]) -> Value {
    let failed = |error: &str| serde_json::json!({ "error": error, "kind": "start" });
    let Some(bridge) = web_sys::window()
        .and_then(|window| Reflect::get(&window, &"cocatSql".into()).ok())
        .filter(JsValue::is_object)
    else {
        return failed("this page has no SQL engine");
    };
    let Ok(run) = Reflect::get(&bridge, &method.into()).and_then(|run| run.dyn_into::<Function>())
    else {
        return failed("this page has no SQL engine");
    };
    let arguments: js_sys::Array = arguments
        .iter()
        .map(|argument| JSON::parse(&argument.to_string()).unwrap_or(JsValue::NULL))
        .collect();
    let Ok(promise) = run
        .apply(&bridge, &arguments)
        .and_then(|answer| answer.dyn_into::<Promise>())
    else {
        return failed("the SQL engine refused the call");
    };
    match wasm_bindgen_futures::JsFuture::from(promise).await {
        Ok(answer) => from_js(&answer),
        Err(error) => failed(&format!("{error:?}")),
    }
}

fn model_context() -> Option<JsValue> {
    let window = web_sys::window()?;
    [
        JsValue::from(window.document()?),
        JsValue::from(window.navigator()),
    ]
    .into_iter()
    .filter_map(|holder| Reflect::get(&holder, &"modelContext".into()).ok())
    .find(|context| context.is_object())
}

fn to_js(value: &Value) -> Option<Object> {
    JSON::parse(&value.to_string()).ok()?.dyn_into().ok()
}

fn from_js(value: &JsValue) -> Value {
    JSON::stringify(value)
        .ok()
        .and_then(|text| text.as_string())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}
