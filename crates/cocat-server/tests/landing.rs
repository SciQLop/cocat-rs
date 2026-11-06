mod ui_support;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use cocat_server::AppState;
use cocat_server::links::Links;
use cocat_server::ws;
use tower::ServiceExt;
use ui_support::{Fixture, body_text};

const PEER: &str = "203.0.113.7:41000";

async fn send(state: AppState, request: Request<Body>) -> Response {
    let mut request = request;
    request
        .extensions_mut()
        .insert(ConnectInfo(PEER.parse::<std::net::SocketAddr>().unwrap()));
    ws::router(state).oneshot(request).await.unwrap()
}

#[tokio::test]
async fn an_anonymous_visitor_reads_the_landing_page() {
    let fixture = Fixture::start().await;
    let response = fixture.get("/", None).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/html; charset=utf-8"
    );
    let body = body_text(response).await;
    for link in [
        "/ui/login",
        "/ui/signup",
        "/docs",
        "https://github.com/SciQLop/SciQLop",
    ] {
        assert!(body.contains(link), "the page names {link}");
    }
    assert!(body.contains("cocat"));
}

#[tokio::test]
async fn the_landing_page_keeps_the_proxy_prefix() {
    let fixture = Fixture::start().await;
    let mut state = fixture.state.clone();
    state.links =
        Links::from_env(&|name| (name == "COCAT_PROXY_PREFIX").then(|| "/cocat-dev".to_owned()))
            .unwrap();

    let response = send(
        state,
        Request::builder()
            .method("GET")
            .uri("/")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    for link in [
        "/cocat-dev/ui/login",
        "/cocat-dev/ui/signup",
        "/cocat-dev/docs",
    ] {
        assert!(body.contains(link), "the page names {link}");
    }
}

#[tokio::test]
async fn a_signed_in_visitor_goes_to_their_rooms() {
    let fixture = Fixture::start().await;
    let id = fixture.verified("landing@example.org").await;
    let token = fixture.token(id).await;

    let response = fixture.get("/", Some(&token)).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        "/ui/rooms"
    );
}

#[tokio::test]
async fn head_answers_like_get() {
    let fixture = Fixture::start().await;
    let response = send(
        fixture.state.clone(),
        Request::builder()
            .method("HEAD")
            .uri("/")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}
