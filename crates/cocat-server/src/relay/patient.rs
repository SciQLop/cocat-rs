//! Downloading from a provider that limits how fast it is asked: on "429 Too Many Requests"
//! the request waits and is sent again, so one busy minute does not fail a whole run.

use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::RETRY_AFTER;

/// How many times a request is sent before a 429 fails the run.
const ATTEMPTS: u32 = 6;

/// The body at `url`, waiting `pause` (doubled each time) or the server's `Retry-After`
/// whenever it answers 429.
pub async fn fetch(http: &reqwest::Client, url: &str, pause: Duration) -> Result<Vec<u8>, String> {
    let mut wait = pause;
    for attempt in 1..=ATTEMPTS {
        let response = http.get(url).send().await.map_err(super::fetch_error)?;
        if response.status() != StatusCode::TOO_MANY_REQUESTS || attempt == ATTEMPTS {
            return body(response).await;
        }
        tokio::time::sleep(retry_after(&response).unwrap_or(wait)).await;
        wait *= 2;
    }
    unreachable!("the last attempt returns")
}

async fn body(response: reqwest::Response) -> Result<Vec<u8>, String> {
    let bytes = response
        .error_for_status()
        .map_err(super::fetch_error)?
        .bytes()
        .await
        .map_err(super::fetch_error)?;
    Ok(bytes.to_vec())
}

/// The delay a 429 asks for, when given in seconds.
fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    let seconds = response.headers().get(RETRY_AFTER)?.to_str().ok()?;
    seconds.trim().parse().ok().map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::get;
    use tokio::net::TcpListener;

    use super::*;

    /// A server answering 429 to the first `busy` requests, then "ok".
    async fn busy_for(busy: u32) -> (String, Arc<AtomicU32>) {
        let asked = Arc::new(AtomicU32::new(0));
        let seen = asked.clone();
        let app = Router::new().route(
            "/",
            get(move || {
                let seen = seen.clone();
                async move {
                    match seen.fetch_add(1, Ordering::SeqCst) < busy {
                        true => (StatusCode::TOO_MANY_REQUESTS, "slow down"),
                        false => (StatusCode::OK, "ok"),
                    }
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, asked)
    }

    #[tokio::test]
    async fn a_busy_provider_is_asked_again_until_it_answers() {
        let (url, asked) = busy_for(2).await;

        let body = fetch(&reqwest::Client::new(), &url, Duration::from_millis(1)).await;

        assert_eq!(body.unwrap(), b"ok");
        assert_eq!(asked.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_provider_busy_for_too_long_fails_the_request() {
        let (url, asked) = busy_for(u32::MAX).await;

        let body = fetch(&reqwest::Client::new(), &url, Duration::from_millis(1)).await;

        assert!(body.unwrap_err().contains("429"));
        assert_eq!(asked.load(Ordering::SeqCst), ATTEMPTS);
    }
}
