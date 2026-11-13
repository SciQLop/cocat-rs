//! Registers the page's tools with an AI agent in the browser, when the browser offers WebMCP
//! (<https://webmachinelearning.github.io/webmcp/>). What the tools answer is in
//! [`crate::agent_tools`]; this only bridges to JavaScript.
//!
//! The API is `document.modelContext`; early Chrome builds and the `@mcp-b/global` polyfill put
//! it on `navigator`, so both are tried. Without either, nothing happens.

use js_sys::{Function, JSON, Object, Promise, Reflect};
use serde_json::Value;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

use crate::agent_tools::{self, OPEN, SEARCH};
use crate::api;
use crate::public::PublicCatalogue;
use crate::views::projects::Projects;

pub fn offer_tools(projects: Projects) {
    let Some(context) = model_context() else {
        return;
    };
    let Ok(register) = Reflect::get(&context, &"registerTool".into()) else {
        return;
    };
    let Some(register) = register.dyn_ref::<Function>() else {
        return;
    };
    for definition in agent_tools::definitions() {
        let Some(name) = definition["name"].as_str().map(str::to_owned) else {
            continue;
        };
        let Some(tool) = to_js(&definition) else {
            continue;
        };
        let execute = Closure::<dyn Fn(JsValue) -> Promise>::new(move |input: JsValue| {
            let (input, name) = (from_js(&input), name.clone());
            future_to_promise(async move {
                // An answer the agent reads says what went wrong; a rejection would not.
                let answer = run(projects, &name, &input)
                    .await
                    .unwrap_or_else(|error| serde_json::json!({ "error": error }));
                to_js(&answer)
                    .map(JsValue::from)
                    .ok_or_else(|| JsValue::from_str("the answer is not JSON"))
            })
        });
        let _ = Reflect::set(&tool, &"execute".into(), execute.as_ref());
        // The tools live as long as the page: nothing to unregister them for.
        execute.forget();
        let _ = register.call1(&context, &tool);
    }
}

async fn run(projects: Projects, name: &str, input: &Value) -> Result<Value, String> {
    let all = api::read::<Vec<PublicCatalogue>>(&projects.api(), &["public"])
        .await
        .map_err(|error| error.to_string())?;
    match name {
        SEARCH => agent_tools::search(&all, input),
        OPEN => {
            let catalogue = agent_tools::chosen(&all, input)?;
            projects.switch(&catalogue.project());
            Ok(serde_json::json!({"opened": catalogue.id, "name": catalogue.name}))
        }
        _ => Err(format!("no tool {name}")),
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
