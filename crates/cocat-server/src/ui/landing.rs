//! The public front door at `/`.

use askama::Template;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};

use crate::AppState;
use crate::ui::frame::{self, Frame, signed_in_user};

const SCIQLOP: &str = "https://github.com/SciQLop/SciQLop";

#[derive(Template)]
#[template(path = "landing.html")]
struct Landing {
    frame: Frame,
    sign_in: String,
    sign_up: String,
    docs: String,
    sciqlop: &'static str,
}

/// Signed-in visitors go to the app; everyone else is asked to sign in.
pub async fn landing(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if signed_in_user(&state, &headers).await.is_some() {
        return Redirect::to(&state.links.path("/app")).into_response();
    }
    frame::page(Landing {
        frame: Frame::new(&state.links, None, "cocat"),
        sign_in: state.links.path("/login"),
        sign_up: state.links.path("/signup"),
        docs: state.links.app("/docs"),
        sciqlop: SCIQLOP,
    })
}
