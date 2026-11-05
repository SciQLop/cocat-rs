//! A per-client budget for the routes that spend a password hash, so a flood of attempts
//! cannot pin the server's CPU.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::AppState;

pub const LOGIN_CALLS: u32 = 10;
pub const LOGIN_PERIOD: Duration = Duration::from_secs(60);
pub const MAX_KEYS: usize = 10_000;

/// What Python's `ratelimit.py` calls the client when the transport reports none.
const UNKNOWN: &str = "unknown";

/// The login budget as the server hands it out: one table shared by every socket, plus the
/// proxy depth that decides which address a request is counted against.
#[derive(Clone)]
pub struct LoginLimit {
    limiter: Arc<RateLimiter>,
    trusted_proxies: usize,
}

impl Default for LoginLimit {
    fn default() -> Self {
        Self::new(0)
    }
}

impl LoginLimit {
    pub fn new(trusted_proxies: usize) -> Self {
        Self {
            limiter: Arc::new(RateLimiter::new(LOGIN_CALLS, LOGIN_PERIOD, MAX_KEYS)),
            trusted_proxies,
        }
    }

    pub fn trusted_proxies(&self) -> usize {
        self.trusted_proxies
    }

    fn limiter(&self) -> &RateLimiter {
        &self.limiter
    }
}

/// Fixed windows per key. In memory and per process: a restart forgives every client, which
/// is what the single-process design wants.
pub struct RateLimiter {
    max_calls: u32,
    period: Duration,
    max_keys: usize,
    windows: Mutex<HashMap<String, (Instant, u32)>>,
}

impl RateLimiter {
    pub fn new(max_calls: u32, period: Duration, max_keys: usize) -> Self {
        Self {
            max_calls,
            period,
            max_keys,
            windows: Mutex::new(HashMap::new()),
        }
    }

    /// Counts one call for `key` and says whether it fits. A window opens at its first call
    /// and is refilled by the first call after `period` has elapsed.
    pub fn hit(&self, key: &str, now: Instant) -> bool {
        let mut windows = self.windows();
        if !windows.contains_key(key) && windows.len() >= self.max_keys {
            self.make_room(&mut windows, now);
        }
        let (mut start, mut calls) = windows.get(key).copied().unwrap_or((now, 0));
        if now.saturating_duration_since(start) >= self.period {
            start = now;
            calls = 0;
        }
        if calls >= self.max_calls {
            return false;
        }
        windows.insert(key.to_owned(), (start, calls + 1));
        true
    }

    /// Drops expired windows, then the oldest, until a new key fits. Only reached when the
    /// table is full, so the scratch vector is not on the hot path.
    fn make_room(&self, windows: &mut HashMap<String, (Instant, u32)>, now: Instant) {
        windows.retain(|_, (start, _)| now.saturating_duration_since(*start) < self.period);
        let overflow = (windows.len() + 1).saturating_sub(self.max_keys);
        if overflow == 0 {
            return;
        }
        let mut by_age: Vec<(Instant, String)> = windows
            .iter()
            .map(|(key, (start, _))| (*start, key.clone()))
            .collect();
        by_age.sort_by_key(|(start, _)| *start);
        for (_, key) in by_age.into_iter().take(overflow) {
            windows.remove(&key);
        }
    }

    fn windows(&self) -> MutexGuard<'_, HashMap<String, (Instant, u32)>> {
        // A panic while counting leaves counters that are still sound to use, and refusing
        // every login for the rest of the process would be the worse failure.
        self.windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Python's `ratelimit.py::client_key`: with `trusted_proxies` hops in front, the address the
/// innermost trusted proxy saw; otherwise the peer itself, because any client can write its
/// own `X-Forwarded-For`.
pub fn client_key(headers: &HeaderMap, peer: Option<SocketAddr>, trusted_proxies: usize) -> String {
    let forwarded = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let hops: Vec<&str> = forwarded
        .split(',')
        .map(str::trim)
        .filter(|hop| !hop.is_empty())
        .collect();
    if trusted_proxies > 0 && hops.len() >= trusted_proxies {
        return hops[hops.len() - trusted_proxies].to_owned();
    }
    match peer {
        Some(address) => address.ip().to_string(),
        None => UNKNOWN.to_owned(),
    }
}

/// Refuses the eleventh login in a minute from one client. Every route keeps its own budget:
/// a flood of logins must not lock a client out of the rest of the API.
pub async fn limit_login(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let limit = &state.login_limit;
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0);
    let key = format!(
        "{}|{}",
        request.uri().path(),
        client_key(request.headers(), peer, limit.trusted_proxies())
    );
    if limit.limiter().hit(&key, Instant::now()) {
        next.run(request).await
    } else {
        (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "detail": "Too Many Requests" })),
        )
            .into_response()
    }
}
