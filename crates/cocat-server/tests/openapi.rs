//! The OpenAPI document and Swagger UI: served, public, complete, and aware of the proxy
//! prefix.

mod ui_support;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use cocat_server::links::Links;
use cocat_server::{openapi, ws};
use tower::ServiceExt;
use ui_support::{Fixture, body_text};

/// `{room_id}` in a route and `{room}` in the access matrix name the same thing.
fn normalize(path: &str) -> String {
    let mut normalized = String::with_capacity(path.len());
    let mut inside = false;
    for character in path.chars() {
        match character {
            '{' => {
                inside = true;
                normalized.push('{');
            }
            '}' => {
                inside = false;
                normalized.push('}');
            }
            _ if inside => {}
            _ => normalized.push(character),
        }
    }
    normalized
}

/// The `(method, path)` pairs the access matrix lists, read from its own source so the two
/// cannot drift apart.
fn matrix_operations() -> Vec<(String, String)> {
    let source = include_str!("access_matrix.rs");
    let mut operations = Vec::new();
    for chunk in source.split("row(").skip(1) {
        let mut literals = chunk.split('"').skip(1).step_by(2);
        let method = literals.next().unwrap_or_default().to_owned();
        let path = literals.next().unwrap_or_default().to_owned();
        if path.starts_with('/') {
            operations.push((method, normalize(&path)));
        }
    }
    operations
}

/// The routes the document is meant to describe: every JSON route plus the two legacy ones.
fn documentable(path: &str) -> bool {
    let normalized = normalize(path);
    normalized == "/api"
        || normalized.starts_with("/api/")
        || normalized.starts_with("/auth/")
        || normalized.starts_with("/users/")
        || normalized == "/rooms"
        || normalized == "/room/{}/users"
}

fn documented_operations(document: &serde_json::Value) -> Vec<(String, String)> {
    let mut operations = Vec::new();
    let Some(paths) = document["paths"].as_object() else {
        return operations;
    };
    for (path, item) in paths {
        let Some(item) = item.as_object() else {
            continue;
        };
        for method in item.keys() {
            let method = method.to_uppercase();
            operations.push((method, normalize(path)));
        }
    }
    operations
}

/// Every `$ref` string anywhere under `value`.
fn references(value: &serde_json::Value, see: &mut impl FnMut(&str)) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                if key == "$ref" {
                    if let Some(reference) = value.as_str() {
                        see(reference);
                    }
                } else {
                    references(value, see);
                }
            }
        }
        serde_json::Value::Array(entries) => {
            for entry in entries {
                references(entry, see);
            }
        }
        _ => {}
    }
}

async fn document_of(fixture: &Fixture) -> serde_json::Value {
    let response = fixture.get("/openapi.json", None).await;
    assert_eq!(response.status(), StatusCode::OK, "the document is served");
    let body = body_text(response).await;
    serde_json::from_str(&body).expect("the document parses as JSON")
}

#[tokio::test]
async fn openapi_json_is_served() {
    let fixture = Fixture::start().await;
    let document = document_of(&fixture).await;
    let version = document["openapi"]
        .as_str()
        .expect("the document names its version");
    assert!(
        version.starts_with("3.1"),
        "the document is OpenAPI 3.1, not {version}"
    );
    assert_eq!(document["info"]["title"], "cocat");
    assert_eq!(
        document["info"]["version"],
        env!("CARGO_PKG_VERSION"),
        "the document carries the crate version"
    );
    assert!(
        document["components"]["securitySchemes"]["cookie"].is_object(),
        "the session cookie is a security scheme"
    );

    let schemas = document["components"]["schemas"]
        .as_object()
        .expect("the document has schemas");
    let mut dangling = Vec::new();
    references(&document["paths"], &mut |reference| {
        let name = reference
            .strip_prefix("#/components/schemas/")
            .expect("a schema reference");
        if !schemas.contains_key(name) {
            dangling.push(name.to_owned());
        }
    });
    assert!(
        dangling.is_empty(),
        "the document references schemas it does not define: {dangling:?}"
    );
}

#[tokio::test]
async fn docs_page_is_served() {
    let fixture = Fixture::start().await;
    let response = fixture.get("/docs", None).await;
    let response = if response.status() == StatusCode::SEE_OTHER {
        let target = response.headers()[header::LOCATION]
            .to_str()
            .expect("a location header")
            .to_owned();
        fixture.get(&target, None).await
    } else {
        response
    };
    assert_eq!(response.status(), StatusCode::OK, "the docs page is served");
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/html",
        "the docs page is HTML"
    );
    let body = body_text(response).await;
    assert!(
        body.contains("swagger-ui"),
        "the docs page loads Swagger UI"
    );
    let initializer = fixture.get("/docs/swagger-initializer.js", None).await;
    assert_eq!(
        initializer.status(),
        StatusCode::OK,
        "the docs page ships its initializer"
    );
    let initializer = body_text(initializer).await;
    assert!(
        initializer.contains("/openapi.json"),
        "the docs page fetches the document: {initializer}"
    );
}

#[tokio::test]
async fn every_json_route_is_documented() {
    let fixture = Fixture::start().await;
    let document = document_of(&fixture).await;
    let documented = documented_operations(&document);

    let mut missing = Vec::new();
    for (method, path) in matrix_operations() {
        if documentable(&path) && !documented.contains(&(method.clone(), path.clone())) {
            missing.push(format!("{method} {path}"));
        }
    }
    assert!(
        missing.is_empty(),
        "the matrix lists routes the document omits: {missing:?}"
    );

    let listed = matrix_operations();
    let mut extra = Vec::new();
    for (method, path) in documented {
        if !listed.contains(&(method.clone(), path.clone())) {
            extra.push(format!("{method} {path}"));
        }
    }
    assert!(
        extra.is_empty(),
        "the document lists routes the matrix does not know: {extra:?}"
    );
}

#[tokio::test]
async fn ui_and_websocket_are_not_documented() {
    let fixture = Fixture::start().await;
    let document = document_of(&fixture).await;
    let paths: Vec<String> = document["paths"]
        .as_object()
        .map(|paths| paths.keys().cloned().collect())
        .unwrap_or_default();
    assert!(
        !paths.iter().any(|path| path.starts_with("/ui")),
        "the rendered pages are not part of the JSON API: {paths:?}"
    );
    assert!(
        !paths.iter().any(|path| path == "/room/{room_id}"),
        "the websocket handshake is not part of the JSON API: {paths:?}"
    );
    assert!(
        !paths
            .iter()
            .any(|path| path == "/docs" || path == "/openapi.json"),
        "the documentation does not document itself: {paths:?}"
    );
}

#[tokio::test]
async fn prefix_reaches_servers_and_docs() {
    let fixture = Fixture::start().await;
    let links =
        Links::from_env(&|name| (name == "COCAT_PROXY_PREFIX").then(|| "/cocat".to_owned()))
            .expect("a prefix alone is a valid link set");

    let document = openapi::document(&links);
    let published = serde_json::to_value(&document).expect("the document serializes");
    assert_eq!(
        published["servers"][0]["url"], "/cocat",
        "the document names the path the proxy adds"
    );

    let response = openapi::router(&links, document.clone())
        .with_state(fixture.state.clone())
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/docs")
                .body(Body::empty())
                .expect("a request"),
        )
        .await
        .expect("the docs router answers");
    body_text(response).await;
    let initializer = openapi::router(&links, document.clone())
        .with_state(fixture.state.clone())
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/docs/swagger-initializer.js")
                .body(Body::empty())
                .expect("a request"),
        )
        .await
        .expect("the docs router answers");
    let initializer = body_text(initializer).await;
    assert!(
        initializer.contains("/cocat/openapi.json"),
        "the docs page fetches the document through the proxy prefix: {initializer}"
    );
}

/// The router the openapi tests live beside still answers its own routes, so a failure here
/// points at the merge rather than at the document.
#[tokio::test]
async fn the_rest_of_the_router_still_answers() {
    let fixture = Fixture::start().await;
    let response = ws::router(fixture.state.clone())
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/openapi.json")
                .body(Body::empty())
                .expect("a request"),
        )
        .await
        .expect("the router answers");
    assert_eq!(response.status(), StatusCode::OK);
}
