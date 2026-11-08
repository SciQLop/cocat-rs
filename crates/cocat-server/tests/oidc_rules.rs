use std::fs;

use axum::Json;
use axum::routing::get;
use cocat_core::role::Role;
use cocat_server::mail::Mailer;
use cocat_server::oidc::{self, Identity, ProviderConfig, SignInError};
use cocat_server::room::RoomManager;
use cocat_server::{AppState, db, links::Links, ratelimit::LoginLimit};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

const ORCID: &str = "orcid";

struct Fixture {
    state: AppState,
    _dir: TempDir,
}

impl Fixture {
    async fn start(mail: bool) -> Self {
        let dir = TempDir::new().unwrap();
        let rooms = dir.path().join("rooms");
        fs::create_dir_all(&rooms).unwrap();
        let postbox = dir.path().join("mail");
        fs::create_dir_all(&postbox).unwrap();
        let pool = db::open(&dir.path().join("test.db")).await.unwrap();
        let public = "https://cocat.example.org";
        let mailer = if mail {
            let lookup = |name: &str| match name {
                "COCAT_MAIL_DIR" => Some(postbox.to_string_lossy().into_owned()),
                "COCAT_MAIL_FROM" => Some("cocat@example.org".to_owned()),
                "COCAT_PUBLIC_URL" => Some(public.to_owned()),
                _ => None,
            };
            Mailer::from_env(&lookup).unwrap()
        } else {
            None
        };
        let links = Links::from_env(&|name| match name {
            "COCAT_PUBLIC_URL" => Some(public.to_owned()),
            _ => None,
        })
        .unwrap();
        let state = AppState {
            pool,
            secret: "test-secret".to_owned(),
            rooms: RoomManager::new(rooms, None),
            before_attach: None,
            login_limit: LoginLimit::default(),
            mailer,
            links,
            oidc: Vec::new(),
            web_dir: None,
        };
        Self { state, _dir: dir }
    }

    fn config(verifies_email: bool) -> ProviderConfig {
        serde_json::from_value(json!({
            "name": ORCID,
            "discovery_url": "https://orcid.example.org",
            "client_id": "client",
            "client_secret": "hunter2",
            "verifies_email": verifies_email,
        }))
        .unwrap()
    }

    async fn sign_in(
        &self,
        config: &ProviderConfig,
        subject: &str,
        email: Option<&str>,
    ) -> Result<db::User, SignInError> {
        oidc::sign_in(
            &self.state,
            config,
            Identity {
                subject: subject.to_owned(),
                email: email.map(str::to_owned),
            },
        )
        .await
    }

    async fn user(&self, email: &str, verified: bool) -> Uuid {
        db::create_user(&self.state.pool, email, "hunter2", false, verified)
            .await
            .unwrap()
    }
}

fn provider_env(providers: &str, public: Option<&str>) -> impl Fn(&str) -> Option<String> {
    let providers = providers.to_owned();
    let public = public.map(str::to_owned);
    move |name: &str| match name {
        "COCAT_OAUTH_PROVIDERS" => Some(providers.clone()),
        "COCAT_PUBLIC_URL" => public.clone(),
        _ => None,
    }
}

#[test]
fn config_parses_python_shape() {
    let env = provider_env(
        r#"[{"name":"orcid","discovery_url":"https://orcid.example.org",
            "client_id":"client","client_secret":"hunter2"}]"#,
        Some("https://cocat.example.org"),
    );

    let providers = oidc::providers_from_env(&env).unwrap();

    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].name, ORCID);
    assert_eq!(providers[0].discovery_url, "https://orcid.example.org");
    assert_eq!(providers[0].client_id, "client");
    assert!(!providers[0].verifies_email);
    assert_eq!(
        providers[0].scopes,
        ["openid".to_owned(), "email".to_owned()]
    );
    assert!(!format!("{:?}", providers[0]).contains("hunter2"));
}

#[test]
fn an_empty_configuration_means_no_provider() {
    let env = provider_env("", None);

    assert!(oidc::providers_from_env(&env).unwrap().is_empty());
}

#[test]
fn bad_configuration_is_refused() {
    for configured in [
        "not json",
        r#"{"name":"orcid"}"#,
        r#"[{"name":"ORCID","discovery_url":"https://orcid.example.org",
             "client_id":"client","client_secret":"hunter2"}]"#,
        r#"[{"name":"a b","discovery_url":"https://orcid.example.org",
             "client_id":"client","client_secret":"hunter2"}]"#,
    ] {
        let env = provider_env(configured, Some("https://cocat.example.org"));
        assert!(
            oidc::providers_from_env(&env).is_err(),
            "{configured} was accepted"
        );
    }
}

#[test]
fn public_url_required() {
    let env = provider_env(
        r#"[{"name":"orcid","discovery_url":"https://orcid.example.org",
            "client_id":"client","client_secret":"hunter2"}]"#,
        None,
    );

    let error = oidc::providers_from_env(&env).unwrap_err();

    assert!(error.to_string().contains("COCAT_PUBLIC_URL"), "{error}");
}

#[tokio::test]
async fn unreachable_provider_skipped() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let document = json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/token"),
        "jwks_uri": format!("{issuer}/jwks"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
    });
    let keys = json!({ "keys": [] });
    let app = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            get(move || {
                let document = document.clone();
                async move { Json(document) }
            }),
        )
        .route(
            "/jwks",
            get(move || {
                let keys = keys.clone();
                async move { Json(keys) }
            }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let up = serde_json::from_value(json!({
        "name": "up",
        "discovery_url": issuer,
        "client_id": "client",
        "client_secret": "hunter2",
    }))
    .unwrap();
    let down = serde_json::from_value(json!({
        "name": "down",
        "discovery_url": "http://127.0.0.1:1",
        "client_id": "client",
        "client_secret": "hunter2",
    }))
    .unwrap();
    let links = Links::from_env(&|name| match name {
        "COCAT_PUBLIC_URL" => Some("https://cocat.example.org".to_owned()),
        _ => None,
    })
    .unwrap();

    let providers = oidc::discover(vec![up, down], &links).await;

    assert_eq!(providers.len(), 1, "the down provider should be skipped");
    assert_eq!(providers[0].config.name, "up");
}

#[tokio::test]
async fn untrusted_email_becomes_placeholder() {
    let fixture = Fixture::start(true).await;
    let config = Fixture::config(false);

    let user = fixture
        .sign_in(&config, "u1", Some("member@example.org"))
        .await
        .unwrap();

    assert_eq!(user.email, "u1@orcid.placeholder.cocat.org");
    assert!(!user.is_verified);
}

#[tokio::test]
async fn invalid_email_becomes_placeholder() {
    let fixture = Fixture::start(true).await;
    let config = Fixture::config(true);

    let user = fixture
        .sign_in(&config, "u1", Some("not an address"))
        .await
        .unwrap();

    assert_eq!(user.email, "u1@orcid.placeholder.cocat.org");
    assert!(!user.is_verified);
}

#[tokio::test]
async fn linked_subject_signs_in() {
    let fixture = Fixture::start(true).await;
    let config = Fixture::config(true);

    let first = fixture
        .sign_in(&config, "u1", Some("member@example.org"))
        .await
        .unwrap();
    let second = fixture
        .sign_in(&config, "u1", Some("other@example.org"))
        .await
        .unwrap();

    assert_eq!(first.id, second.id);
    assert_eq!(second.email, "member@example.org");
    assert_eq!(
        oidc::linked_providers(&fixture.state.pool, first.id)
            .await
            .unwrap(),
        vec![ORCID.to_owned()]
    );
}

#[tokio::test]
async fn trusted_email_links_verified_account() {
    let fixture = Fixture::start(true).await;
    let member = fixture.user("member@example.org", true).await;
    let config = Fixture::config(true);

    let user = fixture
        .sign_in(&config, "u1", Some("member@example.org"))
        .await
        .unwrap();

    assert_eq!(user.id, member);
    assert_eq!(
        oidc::linked_providers(&fixture.state.pool, member)
            .await
            .unwrap(),
        vec![ORCID.to_owned()]
    );
}

#[tokio::test]
async fn trusted_email_unverified_account_exists() {
    let fixture = Fixture::start(true).await;
    let member = fixture.user("member@example.org", false).await;
    let config = Fixture::config(true);

    let error = fixture
        .sign_in(&config, "u1", Some("member@example.org"))
        .await
        .unwrap_err();

    assert!(matches!(error, SignInError::Exists), "{error}");
    assert!(
        oidc::linked_providers(&fixture.state.pool, member)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn untrusted_email_never_links() {
    let fixture = Fixture::start(true).await;
    let member = fixture.user("member@example.org", true).await;
    let config = Fixture::config(false);

    let user = fixture
        .sign_in(&config, "u1", Some("member@example.org"))
        .await
        .unwrap();

    assert_ne!(user.id, member);
    assert_eq!(user.email, "u1@orcid.placeholder.cocat.org");
    assert!(
        oidc::linked_providers(&fixture.state.pool, member)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn trusted_without_mailer_does_not_link() {
    let fixture = Fixture::start(false).await;
    let member = fixture.user("member@example.org", true).await;
    let config = Fixture::config(true);

    let error = fixture
        .sign_in(&config, "u1", Some("member@example.org"))
        .await
        .unwrap_err();

    assert!(matches!(error, SignInError::Exists), "{error}");
    assert!(
        oidc::linked_providers(&fixture.state.pool, member)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn new_user_gets_personal_room() {
    let fixture = Fixture::start(true).await;
    let config = Fixture::config(true);

    let user = fixture
        .sign_in(&config, "u1", Some("member@example.org"))
        .await
        .unwrap();

    let rooms = db::list_user_rooms(&fixture.state.pool, user.id)
        .await
        .unwrap();
    assert_eq!(rooms.len(), 1);
    assert_eq!(rooms[0].0.name, db::PERSONAL_ROOM_NAME);
    assert_eq!(rooms[0].1, Role::Owner);
}

#[tokio::test]
async fn placeholder_never_verified() {
    let fixture = Fixture::start(true).await;
    let config = Fixture::config(false);

    let user = fixture.sign_in(&config, "u1", None).await.unwrap();
    let again = fixture.sign_in(&config, "u1", None).await.unwrap();

    assert_eq!(user.email, "u1@orcid.placeholder.cocat.org");
    assert!(!user.is_verified);
    assert!(!again.is_verified);
}

#[tokio::test]
async fn deleting_the_user_drops_the_link() {
    let fixture = Fixture::start(true).await;
    let config = Fixture::config(true);
    let user = fixture
        .sign_in(&config, "u1", Some("member@example.org"))
        .await
        .unwrap();

    assert!(db::delete_user(&fixture.state.pool, user.id).await.unwrap());

    assert!(
        oidc::linked_providers(&fixture.state.pool, user.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        db::oauth_user(&fixture.state.pool, ORCID, "u1")
            .await
            .unwrap()
            .is_none()
    );
}
