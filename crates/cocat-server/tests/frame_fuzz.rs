//! Hostile websocket frames: whatever a client sends, a viewer cannot change the room, the
//! room thread keeps working, memory stays bounded and the file still reopens.

use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use cocat_core::role::Role;
use cocat_core::store::FileRoomStore;
use cocat_server::room::{ClientId, Inbox, RoomHandle, RoomManager, outbox};
use cocat_server::{AppState, db, lifecycle, links::Links, ratelimit::LoginLimit};
use proptest::prelude::*;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::sync::OnceCell;
use uuid::Uuid;
use yrs::sync::{Message, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, Map, ReadTxn, StateVector, Transact, Update};

const MEMORY_BUDGET: u64 = 256 * 1024 * 1024;
const WAIT: Duration = Duration::from_secs(2);

/// The database and the users cost more to build than a case, so they are made once.
struct World {
    _dir: TempDir,
    updates: PathBuf,
    pool: SqlitePool,
    rooms: RoomManager,
    owner: Uuid,
    editor: Uuid,
    viewer: Uuid,
    baseline_rss: u64,
}

static WORLD: OnceCell<World> = OnceCell::const_new();

impl World {
    async fn get() -> &'static World {
        WORLD
            .get_or_init(|| async {
                let dir = tempfile::tempdir().expect("a temporary directory");
                let updates = dir.path().join("rooms");
                std::fs::create_dir(&updates).expect("a rooms directory");
                let pool = db::open(&dir.path().join("test.db"))
                    .await
                    .expect("a database");
                let owner = db::create_user(&pool, "owner@example.org", "hunter2", false, true)
                    .await
                    .expect("the owner");
                let editor = db::create_user(&pool, "editor@example.org", "hunter2", false, true)
                    .await
                    .expect("the editor");
                let viewer = db::create_user(&pool, "viewer@example.org", "hunter2", false, true)
                    .await
                    .expect("the viewer");
                World {
                    _dir: dir,
                    updates: updates.clone(),
                    pool,
                    rooms: RoomManager::new(updates, None),
                    owner,
                    editor,
                    viewer,
                    baseline_rss: rss_bytes(),
                }
            })
            .await
    }

    fn state(&self) -> AppState {
        AppState {
            pool: self.pool.clone(),
            secret: "test-secret".to_owned(),
            rooms: self.rooms.clone(),
            before_attach: None,
            login_limit: LoginLimit::default(),
            mailer: None,
            links: Links::default(),
            oidc: Vec::new(),
        }
    }

    async fn room(&self) -> (String, RoomHandle) {
        let room = lifecycle::create_room(&self.state(), self.owner, "lab")
            .await
            .expect("a room");
        db::add_member(&self.pool, &room.id, self.editor, Role::Editor)
            .await
            .expect("the editor joins");
        db::add_member(&self.pool, &room.id, self.viewer, Role::Viewer)
            .await
            .expect("the viewer joins");
        let handle = self.rooms.get_or_open(&room.id).expect("a live room");
        let keeper = attach(&handle, 999_999, self.owner, Role::Owner);
        std::mem::forget(keeper);
        (room.id, handle)
    }
}

#[derive(Debug, Clone)]
enum Hostile {
    Step1(Vec<u8>),
    Step1Raw(Vec<u8>),
    Step2(Vec<u8>),
    Update(Vec<u8>),
    Trailing(Vec<u8>),
    Other(u8, Vec<u8>),
    Raw(Vec<u8>),
}

fn payload() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..=512)
}

fn hostile() -> impl Strategy<Value = Hostile> {
    prop_oneof![
        payload().prop_map(Hostile::Step1),
        payload().prop_map(Hostile::Step1Raw),
        payload().prop_map(Hostile::Step2),
        payload().prop_map(Hostile::Update),
        payload().prop_map(Hostile::Trailing),
        (any::<u8>(), payload()).prop_map(|(kind, bytes)| Hostile::Other(kind, bytes)),
        prop::collection::vec(any::<u8>(), 0..=65_536).prop_map(Hostile::Raw),
    ]
}

fn cases() -> u32 {
    std::env::var("FUZZ_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(24)
}

fn encode(hostile: &Hostile) -> Bytes {
    let message = |sync| Bytes::from(Message::Sync(sync).encode_v1());
    match hostile {
        Hostile::Step1(vector) => {
            let vector = std::panic::catch_unwind(|| StateVector::decode_v1(vector))
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default();
            message(SyncMessage::SyncStep1(vector))
        }
        Hostile::Step1Raw(vector) => {
            let mut frame = vec![0, 0];
            frame.extend_from_slice(&varint(vector.len()));
            frame.extend_from_slice(vector);
            Bytes::from(frame)
        }
        Hostile::Step2(update) => message(SyncMessage::SyncStep2(update.clone())),
        Hostile::Update(update) => message(SyncMessage::Update(update.clone())),
        Hostile::Trailing(extra) => {
            let mut frame = message(SyncMessage::SyncStep1(StateVector::default())).to_vec();
            frame.extend_from_slice(extra);
            Bytes::from(frame)
        }
        Hostile::Other(kind, rest) => {
            let mut frame = vec![*kind];
            frame.extend_from_slice(rest);
            Bytes::from(frame)
        }
        Hostile::Raw(bytes) => Bytes::from(bytes.clone()),
    }
}

/// The v1 frame format's length prefix: unsigned LEB128.
fn varint(mut value: usize) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            return out;
        }
    }
}

fn attach(room: &RoomHandle, client: ClientId, user: Uuid, role: Role) -> Inbox {
    let (out, inbox) = outbox();
    let _ = room.attach(client, user, role, out);
    inbox
}

async fn next_frame(inbox: &mut Inbox) -> Option<Bytes> {
    tokio::time::timeout(WAIT, inbox.frames.recv())
        .await
        .ok()
        .flatten()
}

/// Ask the room for its whole state and fold it into a fresh document.
async fn served_state(room: &RoomHandle, client: ClientId, user: Uuid) -> Doc {
    let mut inbox = attach(room, client, user, Role::Editor);
    room.frame(
        client,
        Bytes::from(Message::Sync(SyncMessage::SyncStep1(StateVector::default())).encode_v1()),
    );
    let doc = Doc::new();
    while let Some(frame) = next_frame(&mut inbox).await {
        if let Ok(Message::Sync(SyncMessage::SyncStep2(update))) = Message::decode_v1(&frame) {
            let _ = doc
                .transact_mut()
                .apply_update(Update::decode_v1(&update).expect("a decodable update"));
            break;
        }
    }
    let _ = room.detach(client).await;
    doc
}

fn state_bytes(doc: &Doc) -> Vec<u8> {
    doc.transact()
        .encode_state_as_update_v1(&StateVector::default())
}

fn reopened(updates: &std::path::Path, room_id: &str) -> Doc {
    let (_store, records) = FileRoomStore::open(updates, room_id).expect("a readable room file");
    let doc = Doc::new();
    for record in records {
        let _ = doc
            .transact_mut()
            .apply_update(Update::decode_v1(&record).expect("a stored update"));
    }
    doc
}

fn rss_bytes() -> u64 {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: u64 = statm
        .split_whitespace()
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or_default();
    pages * 4096
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), ..ProptestConfig::default() })]
    #[test]
    fn hostile_frames_leave_the_room_intact(sequence in prop::collection::vec(hostile(), 1..=8)) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        runtime.block_on(async move {
            let world = World::get().await;
            let (room_id, room) = world.room().await;
            let before = state_bytes(&served_state(&room, 1_000_000, world.editor).await);

            let _reader = attach(&room, 1, world.viewer, Role::Viewer);
            for hostile in &sequence {
                room.frame(1, encode(hostile));
            }
            room.flush().await;

            let after = state_bytes(&served_state(&room, 1_000_001, world.editor).await);
            assert_eq!(before, after, "a viewer's frames changed the room: {} -> {}", before.len(), after.len());

            let mut fresh = attach(&room, 2, world.editor, Role::Editor);
            room.frame(
                2,
                Bytes::from(
                    Message::Sync(SyncMessage::SyncStep1(StateVector::default())).encode_v1(),
                ),
            );
            let mut synced = false;
            while let Some(frame) = next_frame(&mut fresh).await {
                if let Ok(Message::Sync(SyncMessage::SyncStep2(_))) = Message::decode_v1(&frame) {
                    synced = true;
                    break;
                }
            }
            assert!(synced, "a fresh client got no sync at all");
            let mut observer = attach(&room, 3, world.viewer, Role::Viewer);
            let _writer = attach(&room, 4, world.editor, Role::Editor);
            let edit = update_with("fuzz", "value");
            room.frame(
                4,
                Bytes::from(Message::Sync(SyncMessage::Update(edit)).encode_v1()),
            );
            let mut seen = false;
            while let Some(frame) = next_frame(&mut observer).await {
                if let Ok(Message::Sync(SyncMessage::Update(_))) = Message::decode_v1(&frame) {
                    seen = true;
                    break;
                }
            }
            assert!(seen, "a valid editor update never reached another client");

            room.flush().await;
            let on_disk = reopened(&world.updates, &room_id);
            let served = served_state(&room, 1_000_002, world.editor).await;
            assert_eq!(
                state_bytes(&on_disk),
                state_bytes(&served),
                "the room file and the room disagree"
            );

            let grown = rss_bytes().saturating_sub(world.baseline_rss);
            assert!(grown < MEMORY_BUDGET, "the process grew by {grown} bytes");

            drop((_reader, observer, _writer));
            let _ = room.delete().await;
        });
    }
}

fn update_with(key: &str, value: &str) -> Vec<u8> {
    let doc = Doc::new();
    doc.get_or_insert_map("content")
        .insert(&mut doc.transact_mut(), key, value);
    doc.transact()
        .encode_state_as_update_v1(&StateVector::default())
}
