//! The OpenAPI document and the Swagger UI that shows it.
//!
//! Both are public: a browser that has never signed in can read the shape of the API, and
//! the document itself says which operations need the session cookie.

use std::sync::Arc;

use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Json, Router};
use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityScheme};
use utoipa::{Modify, OpenApi, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_swagger_ui::Config;

use crate::links::Links;
use crate::ratelimit::LoginLimit;
use crate::{AppState, accounts_api, api, auth};

/// The one error body every JSON route sends: `{"detail": "..."}`.
#[derive(ToSchema)]
pub struct Detail {
    pub detail: String,
}

#[derive(OpenApi)]
#[openapi(
    info(title = "cocat", version = env!("CARGO_PKG_VERSION")),
    components(schemas(Detail)),
    modifiers(&SessionCookie)
)]
struct Root;

/// The session cookie as `apiKey` in a cookie: Swagger UI then sends it if the reader has
/// signed in in the same browser.
struct SessionCookie;

impl Modify for SessionCookie {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_default();
        components.add_security_scheme(
            "cookie",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new("fastapiusersauth"))),
        );
    }
}

/// The JSON API as one value: the routes that answer, and the document that describes them.
///
/// `ws::router` splits it once, so what is served and what is published cannot drift.
pub struct Api {
    pub router: Router<AppState>,
    pub document: utoipa::openapi::OpenApi,
}

/// Build the JSON API: every module's documented router, merged and split.
pub fn api(limit: &LoginLimit, links: &Links) -> Api {
    let (router, mut document) = OpenApiRouter::with_openapi(Root::openapi())
        .merge(api::router())
        .merge(accounts_api::router(limit))
        .merge(auth::router(limit))
        .split_for_parts();
    let prefix = links.app("");
    document.servers = Some(vec![utoipa::openapi::Server::new(if prefix.is_empty() {
        "/".to_owned()
    } else {
        prefix
    })]);
    Api { router, document }
}

/// The document alone, for the tests and for anything that publishes it without serving it.
pub fn document(links: &Links) -> utoipa::openapi::OpenApi {
    api(&LoginLimit::default(), links).document
}

/// `/openapi.json` and the Swagger UI page at `/docs`.
///
/// The proxy prefix is part of the document's server and of the URL the page fetches; the
/// routes themselves stay where the proxy forwards them, unprefixed.
///
/// The page is served through `utoipa_swagger_ui::serve` rather than its `SwaggerUi` router,
/// whose `/docs` → `/docs/` redirect ignores the proxy prefix and would leave the proxy.
pub fn router(links: &Links, document: utoipa::openapi::OpenApi) -> Router<AppState> {
    let config = Arc::new(Config::new([links.app("/openapi.json")]));
    let page = links.app("/docs/");
    let index = Arc::clone(&config);
    Router::new()
        .route(
            "/openapi.json",
            get(move || async move { Json(document.clone()) }),
        )
        .route("/docs", get(move || async move { Redirect::to(&page) }))
        .route(
            "/docs/",
            get(move || async move { swagger_file("", index) }),
        )
        .route(
            "/docs/{*file}",
            get(move |Path(file): Path<String>| async move { swagger_file(&file, config) }),
        )
}

/// One file of the bundled Swagger UI; `index.html` for the page itself.
fn swagger_file(file: &str, config: Arc<Config<'static>>) -> Response {
    match utoipa_swagger_ui::serve(file, config) {
        Ok(Some(file)) if file.gzpipped => (
            [
                (header::CONTENT_TYPE, file.content_type),
                (header::CONTENT_ENCODING, "gzip".to_owned()),
            ],
            file.bytes,
        )
            .into_response(),
        Ok(Some(file)) => ([(header::CONTENT_TYPE, file.content_type)], file.bytes).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::warn!(%error, "cannot serve a Swagger UI file");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
