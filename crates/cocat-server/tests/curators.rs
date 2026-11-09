//! Curators look after the public catalogues: every administrator, plus the accounts an
//! administrator marks, from the admin pages or the command line.

mod ui_support;

use std::path::Path;
use std::process::Command;

use axum::http::StatusCode;
use cocat_server::db;
use tempfile::TempDir;
use ui_support::{Fixture, body_text, location};

#[tokio::test]
async fn administrators_are_curators_and_others_only_once_marked() {
    let fixture = Fixture::start().await;
    let pool = &fixture.state.pool;
    let admin = db::create_user(pool, "admin@example.org", "pwd", true, true)
        .await
        .unwrap();
    let plain = fixture.verified("plain@example.org").await;

    assert!(db::is_curator(pool, admin).await.unwrap());
    assert!(!db::is_curator(pool, plain).await.unwrap());
    assert!(db::set_curator(pool, plain, true).await.unwrap());
    assert!(db::is_curator(pool, plain).await.unwrap());
    assert!(db::set_curator(pool, plain, false).await.unwrap());
    assert!(!db::is_curator(pool, plain).await.unwrap());
    assert!(
        !db::set_curator(pool, uuid::Uuid::new_v4(), true)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn an_administrator_marks_and_unmarks_a_curator_from_the_users_page() {
    let fixture = Fixture::start().await;
    let admin = db::create_user(&fixture.state.pool, "admin@example.org", "pwd", true, true)
        .await
        .unwrap();
    let token = fixture.token(admin).await;
    let user = fixture.verified("user@example.org").await;

    let page = body_text(fixture.get("/ui/admin/users", Some(&token)).await).await;
    assert!(page.contains("Make curator"), "{page}");

    let response = fixture
        .post(
            &format!("/ui/admin/users/{user}/flag"),
            Some(&token),
            "flag=curator&value=true",
        )
        .await;
    assert_eq!(location(&response), "/ui/admin/users");
    assert!(db::is_curator(&fixture.state.pool, user).await.unwrap());
    let page = body_text(fixture.get("/ui/admin/users", Some(&token)).await).await;
    assert!(
        page.contains(r#"<span class="tag">curator</span>"#),
        "{page}"
    );
    assert!(page.contains("Remove curator"), "{page}");

    fixture
        .post(
            &format!("/ui/admin/users/{user}/flag"),
            Some(&token),
            "flag=curator&value=false",
        )
        .await;
    assert!(!db::is_curator(&fixture.state.pool, user).await.unwrap());
}

#[tokio::test]
async fn a_plain_member_cannot_mark_a_curator() {
    let fixture = Fixture::start().await;
    let member = fixture.verified("member@example.org").await;
    let token = fixture.token(member).await;

    let response = fixture
        .post(
            &format!("/ui/admin/users/{member}/flag"),
            Some(&token),
            "flag=curator&value=true",
        )
        .await;
    assert!(response.status().is_redirection());
    assert!(!db::is_curator(&fixture.state.pool, member).await.unwrap());
    assert_eq!(
        fixture.get("/ui/admin/users", Some(&token)).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn the_cli_marks_and_unmarks_a_curator() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let created = cli(
        &db_path,
        &[
            "create-user",
            "--email",
            "curator@example.org",
            "--password",
            "plum orbit lantern cello",
        ],
    );
    assert!(created.status.success());

    let granted = cli(&db_path, &["set-curator", "--email", "curator@example.org"]);
    assert!(
        granted.status.success(),
        "{}",
        String::from_utf8_lossy(&granted.stderr)
    );
    assert!(
        String::from_utf8_lossy(&granted.stdout)
            .contains("User curator@example.org is now a curator"),
        "{}",
        String::from_utf8_lossy(&granted.stdout)
    );
    let pool = db::open(&db_path).await.unwrap();
    let id = db::user_by_email(&pool, "curator@example.org")
        .await
        .unwrap()
        .unwrap()
        .id;
    assert!(db::is_curator(&pool, id).await.unwrap());

    let revoked = cli(
        &db_path,
        &["set-curator", "--email", "curator@example.org", "--revoke"],
    );
    assert!(revoked.status.success());
    assert!(
        String::from_utf8_lossy(&revoked.stdout)
            .contains("User curator@example.org is no longer a curator"),
        "{}",
        String::from_utf8_lossy(&revoked.stdout)
    );
    assert!(!db::is_curator(&pool, id).await.unwrap());

    let unknown = cli(&db_path, &["set-curator", "--email", "nobody@example.org"]);
    assert!(!unknown.status.success());
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("no user with email nobody@example.org")
    );
}

fn cli(db_path: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cocat-server"))
        .args(args)
        .arg("--db_path")
        .arg(db_path)
        .output()
        .unwrap()
}
