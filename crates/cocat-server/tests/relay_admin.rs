//! The relay's schedule and its administration section, with a fake provider.

mod ui_support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::http::StatusCode;
use cocat_core::model::EventRecord;
use cocat_core::time::Timestamp;
use cocat_server::db;
use cocat_server::relay::schedule::{self, Since};
use cocat_server::relay::{Fetched, Provider, store};
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::macros::datetime;
use time::{Duration as Span, OffsetDateTime};
use tokio::sync::Notify;
use ui_support::{Fixture, body_text, form, location};

const SECRET: &str = "s3cr3t-value";

struct Fake {
    runs: Arc<AtomicUsize>,
    started: Arc<Notify>,
    release: Option<Arc<Notify>>,
}

#[async_trait]
impl Provider for Fake {
    fn id(&self) -> &'static str {
        "fake"
    }

    fn source(&self) -> &'static str {
        "Fake source"
    }

    fn default_schedule(&self) -> &'static str {
        "0 3 * * *"
    }

    fn settings(&self) -> &'static [&'static str] {
        &["api_key", "region"]
    }

    async fn fetch(
        &self,
        _http: &reqwest::Client,
        _settings: &Value,
    ) -> Result<Vec<Fetched>, String> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        if let Some(release) = &self.release {
            release.notified().await;
        }
        let day = |n: i64| Timestamp::from_unix_nanos(n * 86_400_000_000_000).unwrap();
        Ok(vec![Fetched {
            key: "shocks".to_owned(),
            folder: "Fake".to_owned(),
            name: "Fake shocks".to_owned(),
            description: String::new(),
            url: "https://fake.example.org/shocks".to_owned(),
            catalogue: "cat-shocks".to_owned(),
            events: vec![EventRecord::new("e1", day(1), day(2))],
        }])
    }
}

struct World {
    fixture: Fixture,
    token: String,
    runs: Arc<AtomicUsize>,
    started: Arc<Notify>,
    release: Option<Arc<Notify>>,
}

impl World {
    async fn start() -> Self {
        Self::build(None).await
    }

    async fn gated() -> Self {
        Self::build(Some(Arc::new(Notify::new()))).await
    }

    async fn build(release: Option<Arc<Notify>>) -> Self {
        let mut fixture = Fixture::start().await;
        let runs = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(Notify::new());
        fixture.state.providers = Arc::new(vec![Arc::new(Fake {
            runs: runs.clone(),
            started: started.clone(),
            release: release.clone(),
        })]);
        let admin = db::create_user(&fixture.state.pool, "admin@example.org", "pwd", true, true)
            .await
            .unwrap();
        let token = fixture.token(admin).await;
        Self {
            fixture,
            token,
            runs,
            started,
            release,
        }
    }

    async fn page(&self) -> String {
        let response = self
            .fixture
            .get("/ui/admin/providers", Some(&self.token))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        body_text(response).await
    }

    async fn save(&self, fields: &[(&str, &str)]) -> String {
        let response = self
            .fixture
            .post("/ui/admin/providers/fake", Some(&self.token), &form(fields))
            .await;
        assert!(response.status().is_redirection());
        assert_eq!(location(&response), "/ui/admin/providers");
        self.fixture.follow(&response, &self.token).await.1
    }

    async fn row(&self) -> store::ProviderRow {
        store::provider(&self.fixture.state.pool, "fake")
            .await
            .unwrap()
            .expect("the provider has a row")
    }

    async fn wait_for_a_finished_run(&self) -> store::ProviderRow {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let row = store::provider(&self.fixture.state.pool, "fake")
                .await
                .unwrap();
            if let Some(row) = row.filter(|row| row.last_run_at.is_some()) {
                return row;
            }
            assert!(Instant::now() < deadline, "the run did not finish");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn last_ok(&self, at: OffsetDateTime) {
        let stamp = at.format(&Rfc3339).unwrap();
        sqlx::query("UPDATE relay_provider SET last_ok_at = ? WHERE id = 'fake'")
            .bind(stamp)
            .execute(&self.fixture.state.pool)
            .await
            .unwrap();
    }
}

#[test]
fn the_next_run_follows_the_cron_line_in_utc() {
    let last = datetime!(2026-11-10 10:00 UTC);
    let now = datetime!(2026-11-10 11:00 UTC);

    assert_eq!(
        schedule::next_due("0 3 * * *", Some(last), now).unwrap(),
        datetime!(2026-11-11 03:00 UTC)
    );
    assert_eq!(
        schedule::next_due("0 3 * * *", Some(datetime!(2026-11-10 03:00 UTC)), now).unwrap(),
        datetime!(2026-11-11 03:00 UTC),
        "a run at the slot itself does not make the same slot due again"
    );
    assert_eq!(schedule::next_due("0 3 * * *", None, now).unwrap(), now);
    assert!(schedule::next_due("every day", Some(last), now).is_err());
}

#[tokio::test]
async fn an_overdue_provider_runs_on_start() {
    let world = World::start().await;
    let state = &world.fixture.state;
    let now = datetime!(2026-11-10 12:00 UTC);

    schedule::run_due(state, now, Since::Success).await;
    assert_eq!(world.runs.load(Ordering::SeqCst), 0, "off by default");

    world
        .save(&[("enabled", "on"), ("schedule", "0 3 * * *")])
        .await;
    world.runs.store(0, Ordering::SeqCst);
    world.last_ok(now - Span::hours(1)).await;
    schedule::run_due(state, now, Since::Success).await;
    assert_eq!(
        world.runs.load(Ordering::SeqCst),
        0,
        "the next slot is not here yet"
    );

    world.last_ok(now - Span::days(3)).await;
    schedule::run_due(state, now, Since::Success).await;
    assert_eq!(world.runs.load(Ordering::SeqCst), 1);
    assert!(world.row().await.last_ok_at.is_some());
}

#[tokio::test]
async fn an_invalid_cron_line_is_refused_and_the_old_one_kept() {
    let world = World::start().await;
    world.save(&[("schedule", "0 4 * * *")]).await;

    let notice = world
        .save(&[("enabled", "on"), ("schedule", "every day")])
        .await;

    assert!(notice.contains("not a valid cron line"), "{notice}");
    let row = world.row().await;
    assert_eq!(row.schedule.as_deref(), Some("0 4 * * *"));
    assert!(!row.enabled, "nothing of the refused form was saved");
}

#[tokio::test]
async fn the_admin_page_lists_the_providers_off_by_default() {
    let world = World::start().await;

    let page = world.page().await;

    assert!(page.contains("Catalogue providers"), "{page}");
    assert!(page.contains("Fake source"), "{page}");
    assert!(page.contains(">off<"), "{page}");
    assert!(page.contains("0 3 * * *"), "{page}");
    assert!(page.contains("Never run"), "{page}");
}

#[tokio::test]
async fn the_key_is_masked_on_the_page() {
    let world = World::start().await;
    let notice = world
        .save(&[
            ("enabled", "on"),
            ("schedule", ""),
            ("setting.api_key", SECRET),
            ("setting.region", "eu"),
        ])
        .await;
    assert!(notice.contains("Saved"), "{notice}");

    let page = world.page().await;
    assert!(!page.contains(SECRET), "{page}");
    assert!(page.contains(">set<"), "{page}");
    assert!(page.contains(">on<"), "{page}");
    assert!(page.contains(r#"value="eu""#), "{page}");

    world
        .save(&[
            ("enabled", "on"),
            ("setting.api_key", ""),
            ("setting.region", ""),
        ])
        .await;
    let stored: Value = serde_json::from_str(&world.row().await.settings).unwrap();
    assert_eq!(stored["api_key"], SECRET, "an empty input keeps the secret");
    assert!(
        stored.get("region").is_none(),
        "an empty plain setting is cleared"
    );
}

#[tokio::test]
async fn refresh_now_runs_the_provider() {
    let world = World::gated().await;
    let response = world
        .fixture
        .post("/ui/admin/providers/fake/refresh", Some(&world.token), "")
        .await;
    assert_eq!(location(&response), "/ui/admin/providers");
    let (_, page) = world.fixture.follow(&response, &world.token).await;
    assert!(page.contains("Refreshing Fake source"), "{page}");

    world.started.notified().await;
    let again = world
        .fixture
        .post("/ui/admin/providers/fake/refresh", Some(&world.token), "")
        .await;
    let (_, page) = world.fixture.follow(&again, &world.token).await;
    assert!(page.contains("already being refreshed"), "{page}");

    world.release.as_ref().unwrap().notify_one();
    let row = world.wait_for_a_finished_run().await;
    assert!(row.last_ok_at.is_some(), "{row:?}");
    assert_eq!(world.runs.load(Ordering::SeqCst), 1);
    let page = world.page().await;
    assert!(page.contains("Worked"), "{page}");
    assert!(page.contains("1 added"), "{page}");
}

#[tokio::test]
async fn a_non_admin_cannot_reach_the_providers() {
    let world = World::start().await;
    let member = world.fixture.verified("member@example.org").await;
    let token = world.fixture.token(member).await;

    let page = world.fixture.get("/ui/admin/providers", Some(&token)).await;
    assert_eq!(page.status(), StatusCode::NOT_FOUND);
    for path in [
        "/ui/admin/providers/fake",
        "/ui/admin/providers/fake/refresh",
    ] {
        let response = world
            .fixture
            .post(path, Some(&token), "enabled=on&schedule=0+4+*+*+*")
            .await;
        assert!(response.status().is_redirection(), "{path}");
        assert_eq!(location(&response), "/ui/rooms", "{path}");
    }
    assert!(
        store::provider(&world.fixture.state.pool, "fake")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(world.runs.load(Ordering::SeqCst), 0);
}
