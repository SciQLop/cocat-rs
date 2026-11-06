"""Build the two Python cocat databases the import tests read.

Run once with the Python test checkout's interpreter:

    /home/jeandet/Documents/prog/cocat-python-tests/.venv/bin/python make_python_dbs.py
"""

import hashlib
import json
import sqlite3
from pathlib import Path

HERE = Path(__file__).parent
HASH = "$2b$10$0aQWymGJbVF/FHfPIDH/UeRJ9lquQk7rS2dC5egTYDuJwtoSu/sgC"
TOKEN = "python-invitation-token"

USER_COLUMNS = """
    id TEXT PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    hashed_password TEXT NOT NULL,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    is_superuser BOOLEAN NOT NULL DEFAULT FALSE,
    is_verified BOOLEAN NOT NULL DEFAULT FALSE,
    rooms JSON NOT NULL DEFAULT '[]'
"""


def released(path):
    db = sqlite3.connect(path)
    db.executescript(
        f"""
        CREATE TABLE user ({USER_COLUMNS});
        CREATE TABLE accesstoken (
            user_id TEXT NOT NULL REFERENCES user(id) ON DELETE CASCADE,
            token VARCHAR(43) PRIMARY KEY,
            created_at TIMESTAMP NOT NULL
        );
        """
    )
    db.executemany(
        "INSERT INTO user (id, email, hashed_password, is_verified, rooms) VALUES (?, ?, ?, ?, ?)",
        [
            ("11111111-1111-1111-1111-111111111111", "alice@example.org", HASH, 0,
             json.dumps(["lab", "solo"])),
            ("22222222-2222-2222-2222-222222222222", "bob@example.org", HASH, 0,
             json.dumps(["lab"])),
            ("33333333-3333-3333-3333-333333333333", "carol@example.org", HASH, 0,
             json.dumps(None)),
        ],
    )
    db.commit()
    db.close()


def rooms_model(path):
    db = sqlite3.connect(path)
    db.executescript(
        f"""
        CREATE TABLE user ({USER_COLUMNS});
        CREATE TABLE accesstoken (
            user_id TEXT NOT NULL REFERENCES user(id) ON DELETE CASCADE,
            token VARCHAR(43) PRIMARY KEY,
            created_at TIMESTAMP NOT NULL
        );
        CREATE TABLE room (
            id VARCHAR(255) PRIMARY KEY,
            name VARCHAR(255) NOT NULL,
            created_at DATETIME NOT NULL,
            created_by CHAR(36) REFERENCES user(id) ON DELETE SET NULL
        );
        CREATE TABLE membership (
            room_id VARCHAR(255) NOT NULL REFERENCES room(id) ON DELETE CASCADE,
            user_id CHAR(36) NOT NULL REFERENCES user(id) ON DELETE CASCADE,
            role VARCHAR(10) NOT NULL,
            joined_at DATETIME NOT NULL,
            PRIMARY KEY (room_id, user_id)
        );
        CREATE TABLE invitation (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            token_hash VARCHAR(64) NOT NULL UNIQUE,
            room_id VARCHAR(255) NOT NULL REFERENCES room(id) ON DELETE CASCADE,
            role VARCHAR(10) NOT NULL,
            email VARCHAR(320),
            created_by CHAR(36) REFERENCES user(id) ON DELETE SET NULL,
            created_at DATETIME NOT NULL,
            expires_at DATETIME NOT NULL,
            revoked BOOLEAN NOT NULL DEFAULT FALSE,
            max_uses INTEGER,
            uses INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE oauth_account (
            id CHAR(36) PRIMARY KEY,
            user_id CHAR(36) NOT NULL REFERENCES user(id) ON DELETE CASCADE,
            oauth_name VARCHAR(100) NOT NULL,
            access_token VARCHAR(1024),
            expires_at INTEGER,
            refresh_token VARCHAR(1024),
            account_id VARCHAR(320) NOT NULL,
            account_email VARCHAR(320)
        );
        """
    )
    alice = "11111111-1111-1111-1111-111111111111"
    bob = "22222222-2222-2222-2222-222222222222"
    db.executemany(
        "INSERT INTO user (id, email, hashed_password, is_verified, rooms) VALUES (?, ?, ?, ?, ?)",
        [(alice, "alice@example.org", HASH, 1, "[]"),
         (bob, "bob@example.org", HASH, 1, "[]")],
    )
    db.execute(
        "INSERT INTO room (id, name, created_at, created_by) VALUES (?, ?, ?, ?)",
        ("lab2", "Lab two", "2026-01-02 03:04:05", alice),
    )
    db.executemany(
        "INSERT INTO membership (room_id, user_id, role, joined_at) VALUES (?, ?, ?, ?)",
        [("lab2", alice, "owner", "2026-01-02 03:04:05"),
         ("lab2", bob, "editor", "2026-01-02 03:04:06")],
    )
    db.execute(
        "INSERT INTO invitation (token_hash, room_id, role, email, created_by, created_at, "
        "expires_at, revoked, max_uses, uses) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        (hashlib.sha256(TOKEN.encode()).hexdigest(), "lab2", "viewer", None, alice,
         "2026-01-02 03:04:07", "2099-01-02 03:04:07", 0, None, 0),
    )
    db.execute(
        "INSERT INTO oauth_account (id, user_id, oauth_name, account_id, account_email) "
        "VALUES (?, ?, ?, ?, ?)",
        ("44444444-4444-4444-4444-444444444444", alice, "orcid", "0000-0002-1825-0097",
         "alice@example.org"),
    )
    db.commit()
    db.close()


if __name__ == "__main__":
    released(HERE / "released.db")
    rooms_model(HERE / "rooms_model.db")
