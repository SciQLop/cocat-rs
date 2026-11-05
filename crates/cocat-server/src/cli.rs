//! The seeding commands: users, rooms and memberships, matching the Python CLI.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use sqlx::SqlitePool;
use thiserror::Error;
use uuid::Uuid;

use cocat_core::role::{Role, UnknownRole};

use crate::db;

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
}

#[derive(Debug, Error)]
pub enum CliError {
    #[error(transparent)]
    Db(#[from] db::DbError),
    #[error("no user with email {0}")]
    NoSuchUser(String),
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
    }
}

async fn create_user(
    db_path: &Path,
    email: &str,
    password: &str,
    is_superuser: bool,
) -> Result<(), CliError> {
    let pool = writable_pool(db_path).await?;
    let id = db::create_user(&pool, email, password, is_superuser).await?;
    println!("User {email} created ({id})");
    Ok(())
}

async fn create_room(
    db_path: &Path,
    owner_email: &str,
    room_id: Option<String>,
    name: Option<String>,
) -> Result<(), CliError> {
    let pool = writable_pool(db_path).await?;
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
    let pool = writable_pool(db_path).await?;
    let user = db::user_by_email(&pool, email)
        .await?
        .ok_or_else(|| CliError::NoSuchUser(email.to_owned()))?;
    db::add_member(&pool, room_id, user.id, role).await?;
    println!("User {email} is {} in room {room_id}", role.as_str());
    Ok(())
}

async fn writable_pool(db_path: &Path) -> Result<SqlitePool, CliError> {
    let pool = db::open_writable(db_path).await?;
    db::ensure_schema(&pool).await?;
    Ok(pool)
}

fn parse_role(value: &str) -> Result<Role, UnknownRole> {
    value.parse()
}
