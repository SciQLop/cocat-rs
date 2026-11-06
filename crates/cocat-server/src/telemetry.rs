//! Prometheus metrics, off unless `COCAT_METRICS_BIND` names an address.
//!
//! The recorder is process-global and labels only ever carry fixed route templates,
//! directions, reasons and kinds: never a room id, an address or a raw URI.

use std::error::Error;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::OnceLock;
use std::time::Duration;

use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;
use metrics::{counter, gauge, histogram};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tokio::task::JoinHandle;

use crate::AppState;

pub const BIND_VAR: &str = "COCAT_METRICS_BIND";

const SAMPLE_PERIOD: Duration = Duration::from_secs(60);

static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("{BIND_VAR} must be an address such as 127.0.0.1:9187, got {value:?}")]
    InvalidBind { value: String },
    #[error("cannot serve {BIND_VAR} on {bind}: {source}")]
    Install {
        bind: SocketAddr,
        source: Box<dyn Error + Send + Sync>,
    },
}

pub fn bind_from_env(
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<SocketAddr>, TelemetryError> {
    let Some(value) = env(BIND_VAR) else {
        return Ok(None);
    };
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Ok(None);
    }
    let mut addresses = value
        .to_socket_addrs()
        .map_err(|_| TelemetryError::InvalidBind {
            value: value.clone(),
        })?;
    addresses
        .next()
        .map(Some)
        .ok_or(TelemetryError::InvalidBind {
            value: value.clone(),
        })
}

pub fn install(bind: SocketAddr) -> Result<(), TelemetryError> {
    let handle = PrometheusBuilder::new()
        .install_recorder()
        .map_err(|source| TelemetryError::Install {
            bind,
            source: Box::new(source),
        })?;
    let listener = std::net::TcpListener::bind(bind)
        .and_then(|listener| listener.set_nonblocking(true).map(|()| listener))
        .and_then(tokio::net::TcpListener::from_std)
        .map_err(|source| TelemetryError::Install {
            bind,
            source: Box::new(source),
        })?;
    let served = handle.clone();
    tokio::spawn(async move {
        let route = axum::routing::get(move || async move { served.render() });
        let _ = axum::serve(listener, axum::Router::new().route("/metrics", route)).await;
    });
    let _ = HANDLE.set(handle);
    Ok(())
}

pub fn render() -> Option<String> {
    HANDLE.get().map(PrometheusHandle::render)
}

pub async fn track_http(request: Request, next: Next) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |path| path.as_str().to_owned());
    let method = request.method().as_str().to_owned();
    let started = std::time::Instant::now();
    let response = next.run(request).await;
    counter!(
        "cocat_http_requests_total",
        "route" => route.clone(),
        "method" => method.clone(),
        "status" => response.status().as_u16().to_string(),
    )
    .increment(1);
    histogram!(
        "cocat_http_request_duration_seconds",
        "route" => route,
        "method" => method,
    )
    .record(started.elapsed().as_secs_f64());
    response
}

pub fn spawn_sampler(state: AppState) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(SAMPLE_PERIOD);
        loop {
            ticker.tick().await;
            sample(&state).await;
        }
    })
}

async fn sample(state: &AppState) {
    if let Ok((total, _, _, _, _)) = crate::db::admin_user_counts(&state.pool).await {
        gauge!("cocat_users").set(total as f64);
    }
    if let Ok(total) = crate::db::count_rooms(&state.pool).await {
        gauge!("cocat_rooms").set(total as f64);
    }
    if let Ok(rooms) = crate::db::admin_rooms(&state.pool).await {
        let bytes: u64 = rooms
            .iter()
            .map(|room| state.rooms.room_size(&room.id))
            .sum();
        gauge!("cocat_room_file_bytes_total").set(bytes as f64);
    }
    let (open, _) = state.rooms.open_counts().await;
    gauge!("cocat_rooms_open").set(open as f64);
}

#[derive(Debug, Clone, Copy)]
pub enum Direction {
    In,
    Out,
}

impl Direction {
    fn label(self) -> &'static str {
        match self {
            Direction::In => "in",
            Direction::Out => "out",
        }
    }
}

pub fn ws_connected(delta: i64) {
    gauge!("cocat_ws_connections").increment(delta as f64);
}

pub fn ws_frame(direction: Direction, bytes: usize) {
    counter!("cocat_ws_frames_total", "direction" => direction.label()).increment(1);
    counter!("cocat_ws_bytes_total", "direction" => direction.label()).increment(bytes as u64);
}

pub fn ws_closed(reason: &'static str) {
    counter!("cocat_ws_closed_total", "reason" => reason).increment(1);
}

pub fn room_opened(seconds: f64) {
    histogram!("cocat_room_open_seconds").record(seconds);
}

pub fn room_flushed(seconds: f64) {
    histogram!("cocat_room_flush_seconds").record(seconds);
}

pub fn refused(kind: &'static str) {
    counter!("cocat_refusals_total", "kind" => kind).increment(1);
}

pub fn mail(outcome: &'static str) {
    counter!("cocat_mail_total", "outcome" => outcome).increment(1);
}

pub fn rooms_open(count: usize) {
    gauge!("cocat_rooms_open").set(count as f64);
}
