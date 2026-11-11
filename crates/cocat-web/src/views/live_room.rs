//! A room of another project or document shown in the page's catalogue view: the original
//! of a link, or the visitor's proposal in a round.

use leptos::prelude::*;
use yrs::TransactionAcqError;

use crate::connection;
use crate::route::Route;
use crate::rows::Row;
use crate::session::RoomConfig;
use crate::state::Room;
use crate::views::catalogue::CataloguePage;
use crate::views::event_panel::{EventPanel, Shown};

/// Opens `config`'s room for the subtree under construction: it becomes the subtree's
/// `Room`, synced until the subtree goes.
pub fn open_room(config: RoomConfig) -> Result<Room, TransactionAcqError> {
    let room = Room::new(config);
    provide_context(room.clone());
    let connection = StoredValue::new_local(connection::connect_project(room.clone())?);
    on_cleanup(move || {
        connection.try_with_value(|connection| connection.close());
    });
    Ok(room)
}

/// The catalogue `shown` of the subtree's room, with its event panel beside it, and
/// `children` above the table.
#[component]
pub fn RoomBrowser(shown: Memo<Route>, children: Children) -> impl IntoView {
    let room = expect_context::<Room>();
    let selected = RwSignal::new(None::<Row>);
    let screen = RwSignal::new(None);
    let panel = Memo::new(move |_| selected.with(Shown::of));
    let (editor, published) = (room.can_write(), room.editing);
    Effect::new(move |_| {
        let editing = panel.with(|shown| shown.event().map(str::to_owned));
        published.set(editing.filter(|_| editor));
    });
    view! {
        <main class="content">
            {children()}
            <CataloguePage shown selected screen/>
        </main>
        <EventPanel selected shown=panel route=shown screen/>
    }
}
