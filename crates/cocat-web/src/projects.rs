//! The visitor's projects (rooms, to the server), and switching the page from one to another
//! without loading it again.

use std::str::FromStr;

use cocat_core::role::Role;
use serde::{Deserialize, Deserializer, de};
use url::Url;

use cocat_core::room_id::path_segment;

use crate::session::RoomConfig;

/// One project as `GET /api/rooms` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    #[serde(deserialize_with = "role")]
    pub role: Role,
}

fn role<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Role, D::Error> {
    let name = String::deserialize(deserializer)?;
    Role::from_str(&name).map_err(de::Error::custom)
}

/// Which of the project menu's actions the visitor's role allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actions {
    pub rename: bool,
    pub leave: bool,
    pub delete: bool,
}

impl Actions {
    /// The owner renames and deletes; everyone else can only leave.
    pub fn of(role: Role) -> Self {
        let owner = role == Role::Owner;
        Self {
            rename: owner,
            leave: !owner,
            delete: owner,
        }
    }
}

impl RoomConfig {
    /// The page's config once it holds `project`: the same visitor and server, another room.
    pub fn for_project(&self, project: &Project) -> Option<Self> {
        Some(Self {
            room_id: project.id.clone(),
            room_name: project.name.clone(),
            role: project.role,
            ws_url: socket_for(&self.ws_url, &project.id)?,
            ..self.clone()
        })
    }
}

/// The server's socket for a room is `<base>/room/<id>`: the open room's URL with its last
/// segment swapped.
fn socket_for(ws_url: &str, room_id: &str) -> Option<String> {
    let mut url = Url::parse(ws_url).ok()?;
    url.path_segments_mut().ok()?.pop().push(room_id);
    Some(url.into())
}

/// What the page does when the browser's back or forward lands on `path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arrival {
    /// Still the open project: only the part after `#` moved.
    Stay,
    Open(Project),
    /// A project the page does not know (left, or not listed yet): the server decides.
    Reload,
}

pub fn arrival(open: &str, path: &str, projects: &[Project]) -> Arrival {
    let Some(segment) = room_in_path(path) else {
        return Arrival::Reload;
    };
    let here = |id: &str| path_segment(id) == segment;
    if here(open) {
        return Arrival::Stay;
    }
    projects
        .iter()
        .find(|project| here(&project.id))
        .map_or(Arrival::Reload, |project| Arrival::Open(project.clone()))
}

/// The room segment of a `…/rooms/<id>/browse` path, as escaped there.
fn room_in_path(path: &str) -> Option<String> {
    let url = Url::parse(PAGE).ok()?.join(path).ok()?;
    let segments: Vec<&str> = url.path_segments()?.collect();
    let [.., "rooms", id, "browse"] = segments.as_slice() else {
        return None;
    };
    Some((*id).to_owned())
}

const PAGE: &str = "http://page.invalid/";

/// The tab's title while `name` is open.
pub fn title(name: &str) -> String {
    format!("{name} · cocat")
}

/// A delete goes ahead once the visitor typed the project's name.
pub fn delete_confirmed(typed: &str, name: &str) -> bool {
    typed.trim() == name.trim()
}

/// `<api>/<segments…>`, each segment escaped.
pub fn api_url(api: &str, segments: &[&str]) -> Option<String> {
    let mut url = Url::parse(api).ok()?;
    url.path_segments_mut()
        .ok()?
        .pop_if_empty()
        .extend(segments);
    Some(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> RoomConfig {
        RoomConfig {
            room_id: "lab".into(),
            room_name: "Solitary Waves".into(),
            role: Role::Owner,
            ws_url: "wss://cocat.example.org/cocat-dev/room/lab".into(),
            api_url: "https://cocat.example.org/cocat-dev/api".into(),
            user_email: "alice@example.org".into(),
        }
    }

    fn project(id: &str, name: &str, role: Role) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            role,
        }
    }

    #[test]
    fn the_api_list_reads_as_projects() {
        let listed: Vec<Project> = serde_json::from_str(
            r#"[{"id":"a1","name":"My catalogues","role":"owner"},
                {"id":"b2","name":"Bepi MSA","role":"viewer"}]"#,
        )
        .unwrap();
        assert_eq!(
            listed,
            vec![
                project("a1", "My catalogues", Role::Owner),
                project("b2", "Bepi MSA", Role::Viewer),
            ]
        );
        assert!(serde_json::from_str::<Project>(r#"{"id":"x","name":"x","role":"boss"}"#).is_err());
    }

    #[test]
    fn the_owner_renames_and_deletes_but_cannot_leave() {
        assert_eq!(
            Actions::of(Role::Owner),
            Actions {
                rename: true,
                leave: false,
                delete: true
            }
        );
    }

    #[test]
    fn editors_and_viewers_can_only_leave() {
        for role in [Role::Editor, Role::Viewer] {
            assert_eq!(
                Actions::of(role),
                Actions {
                    rename: false,
                    leave: true,
                    delete: false
                },
                "{role:?}"
            );
        }
    }

    #[test]
    fn switching_replaces_everything_about_the_project_and_keeps_the_visitor() {
        let switched = config()
            .for_project(&project("bepi", "Bepi MSA", Role::Viewer))
            .unwrap();
        assert_eq!(
            switched,
            RoomConfig {
                room_id: "bepi".into(),
                room_name: "Bepi MSA".into(),
                role: Role::Viewer,
                ws_url: "wss://cocat.example.org/cocat-dev/room/bepi".into(),
                api_url: "https://cocat.example.org/cocat-dev/api".into(),
                user_email: "alice@example.org".into(),
            }
        );
    }

    #[test]
    fn an_imported_room_id_is_escaped_in_its_socket_url() {
        let switched = config()
            .for_project(&project("old lab", "Old", Role::Editor))
            .unwrap();
        assert_eq!(
            switched.ws_url,
            "wss://cocat.example.org/cocat-dev/room/old%20lab"
        );
    }

    #[test]
    fn going_back_to_another_known_project_opens_it() {
        let listed = [
            project("lab", "Solitary Waves", Role::Owner),
            project("bepi", "Bepi MSA", Role::Viewer),
        ];
        assert_eq!(
            arrival("lab", "/cocat-dev/ui/rooms/bepi/browse", &listed),
            Arrival::Open(listed[1].clone())
        );
        assert_eq!(
            arrival("lab", "/cocat-dev/ui/rooms/lab/browse", &listed),
            Arrival::Stay
        );
        assert_eq!(
            arrival("lab", "/ui/rooms/gone/browse", &listed),
            Arrival::Reload
        );
        assert_eq!(arrival("lab", "/ui/app", &listed), Arrival::Reload);
    }

    #[test]
    fn an_escaped_id_in_the_path_is_matched_unescaped() {
        let listed = [project("old lab", "Old", Role::Editor)];
        assert_eq!(
            arrival("lab", "/ui/rooms/old%20lab/browse", &listed),
            Arrival::Open(listed[0].clone())
        );
        let plus = [project("a+b", "Plus", Role::Editor)];
        assert_eq!(
            arrival("lab", "/ui/rooms/a+b/browse", &plus),
            Arrival::Open(plus[0].clone())
        );
    }

    #[test]
    fn the_title_names_the_project() {
        assert_eq!(title("Bepi MSA"), "Bepi MSA · cocat");
    }

    #[test]
    fn a_delete_needs_the_project_name_typed() {
        assert!(delete_confirmed(" Bepi MSA ", "Bepi MSA"));
        assert!(!delete_confirmed("Bepi", "Bepi MSA"));
        assert!(!delete_confirmed("", "Bepi MSA"));
    }

    #[test]
    fn api_routes_escape_each_segment() {
        assert_eq!(
            api_url("https://cocat.example.org/cocat-dev/api", &["rooms"]).unwrap(),
            "https://cocat.example.org/cocat-dev/api/rooms"
        );
        assert_eq!(
            api_url(
                "http://127.0.0.1:9/api/",
                &["rooms", "Bepi MSA?#%", "members"]
            )
            .unwrap(),
            "http://127.0.0.1:9/api/rooms/Bepi%20MSA%3F%23%25/members"
        );
    }
}
