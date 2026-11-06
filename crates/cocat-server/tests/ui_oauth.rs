mod oidc_support;
mod ui_support;

use std::collections::HashMap;

use axum::http::StatusCode;
use axum::response::Response;
use cocat_server::{db, oidc};
use oidc_support::Fake;
use ui_support::{Fixture, PUBLIC_URL, body_text, cookie_cleared, cookie_named, location};
use url::Url;

const MEMBER: &str = "member@example.org";
const PLACEHOLDER: &str = "orcid.placeholder.cocat.org";

async fn started(mailer: bool) -> (Fixture, Fake) {
    let fake = Fake::start().await;
    let mut fixture = if mailer {
        Fixture::start_with_mail().await
    } else {
        Fixture::start_public().await
    };
    let providers = oidc::discover(vec![fake.config(true, "orcid")], &fixture.state.links).await;
    assert_eq!(providers.len(), 1, "the fake provider answered discovery");
    fixture.add_oidc(providers);
    (fixture, fake)
}

struct Attempt {
    state: String,
    cookie: String,
    location: Url,
}

fn query(url: &Url) -> HashMap<String, String> {
    url.query_pairs().into_owned().collect()
}

async fn begin(fixture: &Fixture, fake: &Fake) -> Attempt {
    let response = fixture.get("/ui/oauth/orcid/login", None).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let target = Url::parse(&location(&response)).unwrap();
    let query = query(&target);
    fake.agree(&query["code_challenge"], &query["nonce"]);
    Attempt {
        state: query["state"].clone(),
        cookie: cookie_named(&response, "cocat_oidc").expect("the pending cookie"),
        location: target,
    }
}

async fn callback(fixture: &Fixture, cookie: &str, query: &str) -> Response {
    fixture
        .get_cookies(&format!("/ui/oauth/orcid/callback?{query}"), &[cookie])
        .await
}

#[tokio::test]
async fn login_redirects_to_provider_with_pkce_and_nonce() {
    let (fixture, fake) = started(false).await;

    let attempt = begin(&fixture, &fake).await;

    assert_eq!(attempt.location.path(), "/authorize");
    let query = query(&attempt.location);
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["client_id"], "client");
    assert_eq!(
        query["redirect_uri"],
        format!("{PUBLIC_URL}/ui/oauth/orcid/callback")
    );
    assert!(query["scope"].contains("openid"), "{:?}", query);
    assert_eq!(query["code_challenge_method"], "S256");
    assert!(!query["code_challenge"].is_empty());
    assert!(!query["nonce"].is_empty());
    assert!(!query["state"].is_empty());
}

#[tokio::test]
async fn provider_link_on_login_page() {
    let (fixture, _fake) = started(false).await;

    let page = body_text(fixture.get("/ui/login", None).await).await;

    assert!(page.contains("Sign in with orcid"), "{page}");
    assert!(page.contains(r#"href="/ui/oauth/orcid/login""#), "{page}");
}

#[tokio::test]
async fn good_callback_signs_in() {
    let (fixture, fake) = started(false).await;
    fake.email(Some(MEMBER));
    let attempt = begin(&fixture, &fake).await;

    let done = callback(
        &fixture,
        &attempt.cookie,
        &format!("code=code&state={}", attempt.state),
    )
    .await;

    assert_eq!(done.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&done), "/ui/rooms");
    assert!(cookie_named(&done, "fastapiusersauth").is_some());
    let user = db::user_by_email(&fixture.state.pool, MEMBER)
        .await
        .unwrap()
        .expect("the account exists");
    assert_eq!(
        oidc::linked_providers(&fixture.state.pool, user.id)
            .await
            .unwrap(),
        vec!["orcid".to_owned()]
    );
}

#[tokio::test]
async fn forged_state_400() {
    let (fixture, fake) = started(false).await;
    let attempt = begin(&fixture, &fake).await;

    let response = callback(&fixture, &attempt.cookie, "code=code&state=forged").await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(body_text(response).await.contains("Bad state"));
}

#[tokio::test]
async fn missing_cookie_400() {
    let (fixture, fake) = started(false).await;
    let attempt = begin(&fixture, &fake).await;

    let response = callback(&fixture, "", &format!("code=code&state={}", attempt.state)).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(cookie_cleared(&response, "cocat_oidc"));
}

#[tokio::test]
async fn missing_code_provider_error() {
    let (fixture, fake) = started(false).await;
    let attempt = begin(&fixture, &fake).await;

    let response = callback(
        &fixture,
        &attempt.cookie,
        &format!("state={}", attempt.state),
    )
    .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/ui/login?error=provider");
}

#[tokio::test]
async fn token_endpoint_error() {
    let (fixture, fake) = started(false).await;
    fake.token_error();
    let attempt = begin(&fixture, &fake).await;

    let response = callback(
        &fixture,
        &attempt.cookie,
        &format!("code=code&state={}", attempt.state),
    )
    .await;

    assert_eq!(location(&response), "/ui/login?error=provider");
}

#[tokio::test]
async fn bad_signature() {
    let (fixture, fake) = started(false).await;
    fake.tamper();
    let attempt = begin(&fixture, &fake).await;

    let response = callback(
        &fixture,
        &attempt.cookie,
        &format!("code=code&state={}", attempt.state),
    )
    .await;

    assert_eq!(location(&response), "/ui/login?error=provider");
}

#[tokio::test]
async fn wrong_audience() {
    let (fixture, fake) = started(false).await;
    fake.audience("someone-else");
    let attempt = begin(&fixture, &fake).await;

    let response = callback(
        &fixture,
        &attempt.cookie,
        &format!("code=code&state={}", attempt.state),
    )
    .await;

    assert_eq!(location(&response), "/ui/login?error=provider");
}

#[tokio::test]
async fn expired_token() {
    let (fixture, fake) = started(false).await;
    fake.expires_in(-300);
    let attempt = begin(&fixture, &fake).await;

    let response = callback(
        &fixture,
        &attempt.cookie,
        &format!("code=code&state={}", attempt.state),
    )
    .await;

    assert_eq!(location(&response), "/ui/login?error=provider");
}

#[tokio::test]
async fn wrong_nonce() {
    let (fixture, fake) = started(false).await;
    fake.claim_nonce("another-nonce");
    let attempt = begin(&fixture, &fake).await;

    let response = callback(
        &fixture,
        &attempt.cookie,
        &format!("code=code&state={}", attempt.state),
    )
    .await;

    assert_eq!(location(&response), "/ui/login?error=provider");
}

#[tokio::test]
async fn issuer_mismatch_is_refused() {
    let (fixture, fake) = started(false).await;
    fake.issuer("https://evil.example.org");
    let attempt = begin(&fixture, &fake).await;

    let response = callback(
        &fixture,
        &attempt.cookie,
        &format!("code=code&state={}", attempt.state),
    )
    .await;

    assert_eq!(location(&response), "/ui/login?error=provider");
}

#[tokio::test]
async fn no_email_goes_to_confirm_email() {
    let (fixture, fake) = started(false).await;
    let attempt = begin(&fixture, &fake).await;

    let response = callback(
        &fixture,
        &attempt.cookie,
        &format!("code=code&state={}", attempt.state),
    )
    .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/ui/confirm-email");
    let user = db::user_by_email(&fixture.state.pool, &format!("u1@{PLACEHOLDER}"))
        .await
        .unwrap()
        .expect("the placeholder account exists");
    assert!(!user.is_verified);
}

#[tokio::test]
async fn deactivated_account() {
    let (fixture, fake) = started(true).await;
    fixture.verified(MEMBER).await;
    fixture.deactivate(MEMBER).await;
    fake.email(Some(MEMBER));
    let attempt = begin(&fixture, &fake).await;

    let response = callback(
        &fixture,
        &attempt.cookie,
        &format!("code=code&state={}", attempt.state),
    )
    .await;

    assert_eq!(location(&response), "/ui/login?error=credentials");
}

#[tokio::test]
async fn taken_address_exists() {
    let (fixture, fake) = started(true).await;
    fixture.unverified(MEMBER).await;
    fake.email(Some(MEMBER));
    let attempt = begin(&fixture, &fake).await;

    let response = callback(
        &fixture,
        &attempt.cookie,
        &format!("code=code&state={}", attempt.state),
    )
    .await;

    assert_eq!(location(&response), "/ui/login?error=exists");
}

#[tokio::test]
async fn typed_url_allowed_cross_site_refused() {
    let (fixture, _fake) = started(false).await;

    let typed = fixture.get("/ui/oauth/orcid/login", None).await;
    assert_eq!(typed.status(), StatusCode::SEE_OTHER);

    let bookmarked = fixture
        .get_with_headers("/ui/oauth/orcid/login", &[("sec-fetch-site", "none")])
        .await;
    assert_eq!(bookmarked.status(), StatusCode::SEE_OTHER);

    let cross_site = fixture
        .get_with_headers("/ui/oauth/orcid/login", &[("sec-fetch-site", "cross-site")])
        .await;
    assert_eq!(cross_site.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn login_rate_limited() {
    let (fixture, _fake) = started(false).await;

    for _ in 0..10 {
        let response = fixture.get("/ui/oauth/orcid/login", None).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_ne!(location(&response), "/ui/login?error=too_many");
    }

    let response = fixture.get("/ui/oauth/orcid/login", None).await;

    assert_eq!(location(&response), "/ui/login?error=too_many");
}

#[tokio::test]
async fn redirect_uri_ignores_host_header() {
    let (fixture, _fake) = started(false).await;

    let response = fixture
        .get_with_headers("/ui/oauth/orcid/login", &[("host", "evil.example.org")])
        .await;

    let target = Url::parse(&location(&response)).unwrap();
    assert_eq!(
        query(&target)["redirect_uri"],
        format!("{PUBLIC_URL}/ui/oauth/orcid/callback")
    );
}

#[tokio::test]
async fn account_page_names_the_provider() {
    let (fixture, fake) = started(false).await;
    fake.email(Some(MEMBER));
    let attempt = begin(&fixture, &fake).await;
    let done = callback(
        &fixture,
        &attempt.cookie,
        &format!("code=code&state={}", attempt.state),
    )
    .await;
    let session = cookie_named(&done, "fastapiusersauth").expect("a session cookie");
    let token = session.trim_start_matches("fastapiusersauth=");

    let page = body_text(fixture.get("/ui/account", Some(token)).await).await;

    assert!(page.contains("This account signs in with orcid."), "{page}");
    assert!(!page.contains(r#"name="current""#), "{page}");
}

#[tokio::test]
async fn cookie_cleared_after_callback() {
    let (fixture, fake) = started(false).await;
    let attempt = begin(&fixture, &fake).await;

    let response = callback(
        &fixture,
        &attempt.cookie,
        &format!("code=code&state={}", attempt.state),
    )
    .await;

    assert!(cookie_cleared(&response, "cocat_oidc"));
}
