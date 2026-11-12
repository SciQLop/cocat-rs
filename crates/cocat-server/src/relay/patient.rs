//! Downloading from a provider that limits how fast it is asked, or drops a connection now
//! and then: a passing failure waits and asks again, so one bad minute does not fail a run.

use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::RETRY_AFTER;

/// How many times a request is sent before a passing failure fails the run.
const ATTEMPTS: u32 = 6;
/// The longest pause between two tries, whatever the provider's `Retry-After` asks: six
/// tries then end within half an hour.
const MAX_WAIT: Duration = Duration::from_secs(300);
/// The largest answer read: the biggest list today (LASCO, ~20 MB) with ample room, and a
/// bound on what a misbehaving provider can make the server hold.
const MAX_BODY: usize = 512 * 1024 * 1024;

/// What one try gave: the answer (body or a lasting refusal), or a passing failure.
enum Try {
    Done(Result<Vec<u8>, String>),
    Again {
        error: String,
        wait: Option<Duration>,
    },
}

/// The body at `url`. A 429, a 5xx or a broken connection is asked again after the server's
/// `Retry-After`, else after `pause` doubled each time; any other refusal fails at once.
pub async fn fetch(http: &reqwest::Client, url: &str, pause: Duration) -> Result<Vec<u8>, String> {
    fetch_within(http, url, pause, MAX_BODY).await
}

async fn fetch_within(
    http: &reqwest::Client,
    url: &str,
    pause: Duration,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let mut pause = pause;
    let mut last = String::new();
    for _ in 0..ATTEMPTS {
        match attempt(http, url, limit).await {
            Try::Done(answer) => return answer,
            Try::Again { error, wait: asked } => {
                tokio::time::sleep(wait(asked, pause)).await;
                pause *= 2;
                last = error;
            }
        }
    }
    Err(last)
}

/// The pause before the next try: what the provider asked, else ours, never over `MAX_WAIT`.
fn wait(asked: Option<Duration>, pause: Duration) -> Duration {
    asked.unwrap_or(pause).min(MAX_WAIT)
}

async fn attempt(http: &reqwest::Client, url: &str, limit: usize) -> Try {
    let response = match http.get(url).send().await {
        Ok(response) => response,
        Err(error) => return again(super::fetch_error(error), None),
    };
    let status = response.status();
    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        let wait = retry_after(&response);
        return again(status.to_string(), wait);
    }
    match response.error_for_status() {
        Err(refused) => Try::Done(Err(super::fetch_error(refused))),
        Ok(response) => read_within(response, limit).await,
    }
}

async fn read_within(mut response: reqwest::Response, limit: usize) -> Try {
    let too_large = || Try::Done(Err(format!("the answer is larger than {limit} bytes")));
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return too_large();
    }
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if body.len() + chunk.len() > limit => return too_large(),
            Ok(Some(chunk)) => body.extend_from_slice(&chunk),
            Ok(None) => return Try::Done(Ok(body)),
            Err(error) => return again(super::fetch_error(error), None),
        }
    }
}

fn again(error: String, wait: Option<Duration>) -> Try {
    Try::Again { error, wait }
}

/// The delay a 429 or a 503 asks for, when given in seconds.
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
        failing_for(busy, StatusCode::TOO_MANY_REQUESTS).await
    }

    /// A server answering `status` to the first `times` requests, then "ok".
    async fn failing_for(times: u32, status: StatusCode) -> (String, Arc<AtomicU32>) {
        let asked = Arc::new(AtomicU32::new(0));
        let seen = asked.clone();
        let app = Router::new().route(
            "/",
            get(move || {
                let seen = seen.clone();
                async move {
                    match seen.fetch_add(1, Ordering::SeqCst) < times {
                        true => (status, "not now"),
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

    #[tokio::test]
    async fn a_provider_that_fails_for_a_moment_is_asked_again() {
        let (url, asked) = failing_for(1, StatusCode::SERVICE_UNAVAILABLE).await;

        let body = fetch(&reqwest::Client::new(), &url, Duration::from_millis(1)).await;

        assert_eq!(body.unwrap(), b"ok");
        assert_eq!(asked.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_provider_cannot_make_a_run_wait_for_hours() {
        let asked = Some(Duration::from_secs(86_400));

        assert_eq!(wait(asked, Duration::from_secs(30)), MAX_WAIT);
        assert_eq!(wait(None, Duration::from_secs(30)), Duration::from_secs(30));
        assert_eq!(
            wait(Some(Duration::from_secs(5)), Duration::from_secs(30)),
            Duration::from_secs(5)
        );
    }

    #[tokio::test]
    async fn an_answer_over_the_size_cap_fails_without_being_held_whole() {
        let app = Router::new().route("/", get(|| async { "x".repeat(4096) }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let error = fetch_within(
            &reqwest::Client::new(),
            &url,
            Duration::from_millis(1),
            1024,
        )
        .await
        .unwrap_err();

        assert!(error.contains("larger than"), "{error}");
    }

    #[tokio::test]
    async fn a_request_the_provider_refuses_is_not_sent_again() {
        let (url, asked) = failing_for(1, StatusCode::BAD_REQUEST).await;

        let body = fetch(&reqwest::Client::new(), &url, Duration::from_millis(1)).await;

        assert!(body.unwrap_err().contains("400"));
        assert_eq!(asked.load(Ordering::SeqCst), 1);
    }
}
