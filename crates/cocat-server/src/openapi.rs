//! The OpenAPI document and the Swagger UI that shows it.
//!
//! Both are public: a browser that has never signed in can read the shape of the API, and
//! the document itself says which operations need the session cookie.

use axum::Json;
use axum::Router;
use axum::routing::get;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityScheme};
use utoipa::{Modify, OpenApi, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_swagger_ui::SwaggerUi;

use crate::links::Links;
use crate::{AppState, accounts_api, api, auth, catalogues_api, groups_api, invitations_api};

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

/// The document as this deployment serves it: the routes of every JSON module, and the one
/// server entry a reverse proxy needs.
pub fn document(links: &Links) -> utoipa::openapi::OpenApi {
    let mut document = OpenApiRouter::with_openapi(Root::openapi())
        .merge(api::documented())
        .merge(accounts_api::documented())
        .merge(catalogues_api::documented())
        .merge(groups_api::documented())
        .merge(invitations_api::documented())
        .merge(auth::documented())
        .into_openapi();
    let prefix = links.app("");
    document.servers = Some(vec![utoipa::openapi::Server::new(if prefix.is_empty() {
        "/".to_owned()
    } else {
        prefix
    })]);
    document
}

/// `/openapi.json` and the Swagger UI page at `/docs`.
///
/// The proxy prefix is part of the document's server and of the URL the page fetches; the
/// routes themselves stay where the proxy forwards them, unprefixed.
pub fn router(links: &Links) -> Router<AppState> {
    let document = document(links);
    let served = document.clone();
    let fetch = links.app("/openapi.json");
    Router::new()
        .route(
            "/openapi.json",
            get(move || async move { Json(served.clone()) }),
        )
        .merge(SwaggerUi::new("/docs").config(utoipa_swagger_ui::Config::new([fetch])))
}
