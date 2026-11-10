mod ui_support;

use std::net::SocketAddr;
use std::time::Duration;

use axum::http::StatusCode;
use cocat_server::telemetry::Direction;
use cocat_server::{db, telemetry};
use ui_support::Fixture;

fn parse(value: &str) -> SocketAddr {
    value.parse().expect("an address")
}

#[test]
fn the_bind_comes_from_the_environment() {
    assert_eq!(telemetry::bind_from_env(&|_| None).unwrap(), None);
    assert_eq!(
        telemetry::bind_from_env(&|_| Some(String::new())).unwrap(),
        None
    );
    assert_eq!(
        telemetry::bind_from_env(&|_| Some("127.0.0.1:9187".to_owned())).unwrap(),
        Some(parse("127.0.0.1:9187"))
    );

    let error = telemetry::bind_from_env(&|_| Some("not an address".to_owned())).unwrap_err();
    assert!(
        error.to_string().contains("COCAT_METRICS_BIND"),
        "the error names the variable: {error}"
    );
}

#[tokio::test]
async fn every_signal_is_named_and_no_identifier_is_a_label() {
    telemetry::install(parse("127.0.0.1:0"), None).expect("the recorder installs once");

    let fixture = Fixture::start().await;
    let user = fixture.verified("metrics@example.org").await;
    let token = fixture.token(user).await;
    db::create_room(
        &fixture.state.pool,
        user,
        "metric-room",
        "Metric room",
        db::ProjectKind::Catalogues,
    )
    .await
    .expect("the room is created");

    let me = fixture.get("/api/me", Some(&token)).await;
    assert_eq!(me.status(), StatusCode::OK);
    let members = fixture
        .get("/api/rooms/metric-room/members", Some(&token))
        .await;
    assert_eq!(members.status(), StatusCode::OK);
    let page = fixture.get("/ui/login", None).await;
    assert_eq!(page.status(), StatusCode::OK);
    fixture
        .state
        .rooms
        .read("metric-room", |_doc| ())
        .await
        .expect("the cold read succeeds");

    let mut last = None;
    for _ in 0..11 {
        last = Some(
            fixture
                .post(
                    "/auth/jwt/login",
                    None,
                    "username=metrics%40example.org&password=wrong",
                )
                .await,
        );
    }
    assert_eq!(
        last.expect("eleven attempts").status(),
        StatusCode::TOO_MANY_REQUESTS,
        "the eleventh login is refused and counted"
    );

    telemetry::ws_connected(1);
    telemetry::ws_frame(Direction::In, 64);
    telemetry::ws_frame(Direction::Out, 128);
    telemetry::ws_closed("revoked");
    telemetry::room_flushed(0.25);
    telemetry::mail("sent");

    let sampler = telemetry::spawn_sampler(fixture.state.clone());
    tokio::time::sleep(Duration::from_millis(200)).await;
    sampler.abort();

    let text = telemetry::render().expect("the recorder is installed");
    for name in [
        "cocat_http_requests_total",
        "cocat_http_request_duration_seconds",
        "cocat_ws_connections",
        "cocat_ws_frames_total",
        "cocat_ws_bytes_total",
        "cocat_ws_closed_total",
        "cocat_rooms_open",
        "cocat_room_open_seconds",
        "cocat_room_flush_seconds",
        "cocat_room_file_bytes_total",
        "cocat_refusals_total",
        "cocat_mail_total",
        "cocat_users",
        "cocat_rooms",
    ] {
        assert!(text.contains(name), "the scrape names {name}");
    }

    assert!(
        text.contains("route=\"/api/rooms/{room_id}/members\""),
        "the route label is the template, not the URI"
    );
    assert!(
        text.contains("kind=\"rate_limit\""),
        "the refusal is labelled"
    );
    assert!(
        text.contains("direction=\"in\""),
        "the frame direction is labelled"
    );
    assert!(
        text.contains("reason=\"revoked\""),
        "the close reason is labelled"
    );
    assert!(
        text.contains("outcome=\"sent\""),
        "the mail outcome is labelled"
    );

    for secret in ["metric-room", "metrics@example.org"] {
        assert!(!text.contains(secret), "no label carries {secret}: {text}");
    }
}

#[test]
fn the_endpoint_token_comes_from_the_environment() {
    assert_eq!(
        cocat_server::telemetry::token_from_env(&|_| None).unwrap(),
        None
    );
    assert_eq!(
        cocat_server::telemetry::token_from_env(&|_| Some(String::new())).unwrap(),
        None
    );
    assert_eq!(
        cocat_server::telemetry::token_from_env(&|_| Some("0123456789abcdef".to_owned()))
            .unwrap()
            .as_deref(),
        Some("0123456789abcdef")
    );

    let error = cocat_server::telemetry::token_from_env(&|_| Some("too short".to_owned()))
        .expect_err("a short token is refused");
    assert!(
        error.to_string().contains("COCAT_METRICS_TOKEN"),
        "the error names the variable: {error}"
    );
}

async fn scrape(token: Option<&str>, auth: Option<&str>) -> axum::response::Response {
    use tower::ServiceExt;

    let router = cocat_server::telemetry::metrics_router(token.map(ToOwned::to_owned));
    let mut request = axum::http::Request::builder().uri("/metrics");
    if let Some(value) = auth {
        request = request.header("authorization", value);
    }
    router
        .oneshot(request.body(axum::body::Body::empty()).expect("a request"))
        .await
        .expect("the router answers")
}

#[tokio::test]
async fn the_metrics_endpoint_checks_the_bearer_token() {
    let token = "0123456789abcdef";
    assert_eq!(
        scrape(Some(token), Some("Bearer 0123456789abcdef"))
            .await
            .status(),
        StatusCode::OK,
        "the right token is let in"
    );

    let wrong = scrape(Some(token), Some("Bearer 0123456789abcdee")).await;
    assert_eq!(
        wrong.status(),
        StatusCode::UNAUTHORIZED,
        "a wrong token is refused"
    );
    assert_eq!(
        wrong
            .headers()
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer"),
        "the refusal names the scheme"
    );

    assert_eq!(
        scrape(Some(token), None).await.status(),
        StatusCode::UNAUTHORIZED,
        "a missing token is refused"
    );
    assert_eq!(
        scrape(None, None).await.status(),
        StatusCode::OK,
        "without a configured token the endpoint stays open"
    );
}
