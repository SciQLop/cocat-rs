//! The seeding commands: users, rooms and memberships, matching the Python CLI.

use std::env::VarError;
use std::fs;
use std::future::Future;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use axum::serve::ListenerExt;
use clap::{Parser, Subcommand};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::signal::unix::SignalKind;
use uuid::Uuid;

use cocat_core::role::{Role, UnknownRole};

use crate::links::{Links, LinksError};
use crate::mail::{MailError, Mailer};
use crate::ratelimit::LoginLimit;
use crate::room::RoomManager;
use crate::{AppState, auth, db, ws};

#[derive(Debug, Parser)]
#[command(
    name = "cocat-server",
    about = "Serve cocat rooms and seed their database"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a user with an argon2 password.
    CreateUser {
        #[arg(long)]
        email: String,
        #[arg(long)]
        password: String,
        #[arg(long)]
        is_superuser: bool,
        #[arg(long = "db_path", default_value = "./test.db")]
        db_path: PathBuf,
    },
    /// Create a room owned by a user.
    CreateRoom {
        #[arg(long)]
        owner: String,
        #[arg(long = "room-id")]
        room_id: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long = "db_path", default_value = "./test.db")]
        db_path: PathBuf,
    },
    /// Add a user to an existing room.
    AddUserToRoom {
        #[arg(long)]
        email: String,
        #[arg(long = "room_id")]
        room_id: String,
        #[arg(long, default_value = "editor", value_parser = parse_role)]
        role: Role,
        #[arg(long = "db_path", default_value = "./test.db")]
        db_path: PathBuf,
    },
    /// Remove a user from a room.
    RemoveUserFromRoom {
        #[arg(long)]
        email: String,
        #[arg(long = "room_id")]
        room_id: String,
        #[arg(long = "db_path", default_value = "./test.db")]
        db_path: PathBuf,
    },
    /// Make a room member the owner of the room.
    SetRoomOwner {
        #[arg(long)]
        email: String,
        #[arg(long = "room_id")]
        room_id: String,
        #[arg(long = "db_path", default_value = "./test.db")]
        db_path: PathBuf,
    },
    /// Serve room websockets until SIGINT or SIGTERM.
    Serve {
        #[arg(long, default_value = "localhost")]
        host: String,
        #[arg(long, default_value_t = 8000)]
        port: u16,
        #[arg(long = "update_dir")]
        update_dir: Option<PathBuf>,
        #[arg(long = "db_path", default_value = "./test.db")]
        db_path: PathBuf,
    },
}

#[derive(Debug, Error)]
pub enum CliError {
    #[error(transparent)]
    Db(#[from] db::DbError),
    #[error(transparent)]
    Auth(#[from] auth::AuthError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("no user with email {0}")]
    NoSuchUser(String),
    #[error("COCAT_ROOM_MEMORY_BUDGET must be a positive number of megabytes, got {0:?}")]
    InvalidBudget(String),
    #[error("COCAT_TRUSTED_PROXIES must be a non-negative integer, got {0:?}")]
    InvalidTrustedProxies(String),
    #[error(transparent)]
    Mail(#[from] MailError),
    #[error(transparent)]
    Links(#[from] LinksError),
}

pub async fn run() -> Result<(), CliError> {
    match Cli::parse().command {
        Command::CreateUser {
            email,
            password,
            is_superuser,
            db_path,
        } => create_user(&db_path, &email, &password, is_superuser).await,
        Command::CreateRoom {
            owner,
            room_id,
            name,
            db_path,
        } => create_room(&db_path, &owner, room_id, name).await,
        Command::AddUserToRoom {
            email,
            room_id,
            role,
            db_path,
        } => add_user_to_room(&db_path, &email, &room_id, role).await,
        Command::RemoveUserFromRoom {
            email,
            room_id,
            db_path,
        } => remove_user_from_room(&db_path, &email, &room_id).await,
        Command::SetRoomOwner {
            email,
            room_id,
            db_path,
        } => set_room_owner(&db_path, &email, &room_id).await,
        Command::Serve {
            host,
            port,
            update_dir,
            db_path,
        } => {
            serve(
                &host,
                port,
                update_dir.as_deref().unwrap_or(Path::new("")),
                &db_path,
            )
            .await
        }
    }
}

async fn create_user(
    db_path: &Path,
    email: &str,
    password: &str,
    is_superuser: bool,
) -> Result<(), CliError> {
    let pool = db::open(db_path).await?;
    let id = db::create_user(&pool, email, password, is_superuser).await?;
    db::create_personal_room(&pool, id).await?;
    println!("User {email} created ({id})");
    Ok(())
}

async fn create_room(
    db_path: &Path,
    owner_email: &str,
    room_id: Option<String>,
    name: Option<String>,
) -> Result<(), CliError> {
    let pool = db::open(db_path).await?;
    let owner = db::user_by_email(&pool, owner_email)
        .await?
        .ok_or_else(|| CliError::NoSuchUser(owner_email.to_owned()))?;
    let room_id = room_id.unwrap_or_else(|| Uuid::new_v4().simple().to_string());
    let name = name.unwrap_or_else(|| room_id.clone());
    db::create_room(&pool, owner.id, &room_id, &name).await?;
    println!("{room_id}");
    Ok(())
}

async fn add_user_to_room(
    db_path: &Path,
    email: &str,
    room_id: &str,
    role: Role,
) -> Result<(), CliError> {
    let pool = db::open(db_path).await?;
    let user = db::user_by_email(&pool, email)
        .await?
        .ok_or_else(|| CliError::NoSuchUser(email.to_owned()))?;
    let granted = db::add_member(&pool, room_id, user.id, role).await?;
    println!("User {email} is {} in room {room_id}", granted.as_str());
    Ok(())
}

async fn remove_user_from_room(db_path: &Path, email: &str, room_id: &str) -> Result<(), CliError> {
    let pool = db::open(db_path).await?;
    let user = db::user_by_email(&pool, email)
        .await?
        .ok_or_else(|| CliError::NoSuchUser(email.to_owned()))?;
    if !db::remove_member(&pool, room_id, user.id).await? {
        return Err(db::DbError::NoSuchMembership {
            room_id: room_id.to_owned(),
            user_id: user.id,
        }
        .into());
    }
    println!("User {email} is no longer in room {room_id}");
    Ok(())
}

async fn set_room_owner(db_path: &Path, email: &str, room_id: &str) -> Result<(), CliError> {
    let pool = db::open(db_path).await?;
    let user = db::user_by_email(&pool, email)
        .await?
        .ok_or_else(|| CliError::NoSuchUser(email.to_owned()))?;
    db::set_owner(&pool, room_id, user.id).await?;
    println!("User {email} owns room {room_id}");
    Ok(())
}

async fn serve(host: &str, port: u16, update_dir: &Path, db_path: &Path) -> Result<(), CliError> {
    let budget = room_budget()?;
    let trusted_proxies = trusted_proxies()?;
    let env = |name: &str| std::env::var(name).ok();
    let mailer = Mailer::from_env(&env)?;
    let links = Links::from_env(&env)?;
    if !update_dir.as_os_str().is_empty() {
        fs::create_dir_all(update_dir)?;
    }
    let pool = db::open(db_path).await?;
    let secret = auth::load_secret(db_path)?;
    let rooms = RoomManager::new(update_dir.to_owned(), budget);
    let state = AppState {
        pool,
        secret,
        rooms: rooms.clone(),
        before_attach: None,
        login_limit: LoginLimit::new(trusted_proxies),
        mailer,
        links,
    };

    let listener = TcpListener::bind((host, port)).await?;
    let shutdown = shutdown_signal();
    println!("listening on {}", listener.local_addr()?);
    io::stdout().flush()?;

    axum::serve(
        listener.tap_io(ws::disable_nagle),
        ws::router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await?;
    rooms.flush_all().await;
    Ok(())
}

/// `COCAT_ROOM_MEMORY_BUDGET` is a positive number of megabytes; unset or empty means the
/// server opens rooms without a limit.
fn room_budget() -> Result<Option<u64>, CliError> {
    let value = match std::env::var("COCAT_ROOM_MEMORY_BUDGET") {
        Ok(value) => value,
        Err(VarError::NotPresent) => return Ok(None),
        Err(VarError::NotUnicode(_)) => {
            return Err(CliError::InvalidBudget("a non-unicode value".to_owned()));
        }
    };
    if value.trim().is_empty() {
        return Ok(None);
    }
    parse_budget(&value).map(Some)
}

fn parse_budget(value: &str) -> Result<u64, CliError> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|megabytes| *megabytes > 0)
        .map(|megabytes| megabytes.saturating_mul(1024 * 1024))
        .ok_or_else(|| CliError::InvalidBudget(value.to_owned()))
}

/// `COCAT_TRUSTED_PROXIES` is how many proxies stand in front of the server; unset or empty
/// means the socket address is the client.
fn trusted_proxies() -> Result<usize, CliError> {
    let value = match std::env::var("COCAT_TRUSTED_PROXIES") {
        Ok(value) => value,
        Err(VarError::NotPresent) => return Ok(0),
        Err(VarError::NotUnicode(_)) => {
            return Err(CliError::InvalidTrustedProxies(
                "a non-unicode value".to_owned(),
            ));
        }
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(0);
    }
    value
        .parse::<usize>()
        .ok()
        .filter(|_| value.chars().all(|digit| digit.is_ascii_digit()))
        .ok_or_else(|| CliError::InvalidTrustedProxies(value.to_owned()))
}

/// Registers SIGINT and SIGTERM when it is called, not when it is first polled: the server
/// prints its address before serving, and a caller that signals on that line would
/// otherwise hit the default action and die instead of shutting down gracefully.
fn shutdown_signal() -> impl Future<Output = ()> {
    let interrupt = tokio::signal::unix::signal(SignalKind::interrupt());
    let terminate = tokio::signal::unix::signal(SignalKind::terminate());
    async move {
        let (mut interrupt, mut terminate) = match (interrupt, terminate) {
            (Ok(interrupt), Ok(terminate)) => (interrupt, terminate),
            _ => {
                tracing::warn!("cannot listen for SIGINT and SIGTERM");
                return std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
    }
}

fn parse_role(value: &str) -> Result<Role, UnknownRole> {
    value.parse()
}
