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
    /// Whether the project's editors may share it. A created or renamed project's answer does
    /// not say; a new project lets them.
    #[serde(default = "allowed")]
    pub editors_can_share: bool,
    /// The visitor's own project, "My catalogues", which never gains people.
    #[serde(default)]
    pub personal: bool,
    /// The project holds a SciQLop workspace's files besides its catalogues.
    #[serde(default)]
    pub files: bool,
}

fn allowed() -> bool {
    true
}

pub(crate) fn role<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Role, D::Error> {
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
    /// The owner renames and deletes, editors rename and leave, viewers only leave.
    pub fn of(role: Role) -> Self {
        let owner = role == Role::Owner;
        Self {
            rename: role.can_write(),
            leave: !owner,
            delete: owner,
        }
    }

    /// A public catalogue has no owner and no members: curators rename and delete it.
    pub fn of_public(role: Role) -> Self {
        Self {
            rename: role.can_write(),
            leave: false,
            delete: role.can_write(),
        }
    }

    /// The actions on the project with id `id`.
    pub fn on(id: &str, role: Role) -> Self {
        match cocat_core::room_id::is_public_id(id) {
            true => Self::of_public(role),
            false => Self::of(role),
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
            files: project.files,
            ..self.clone()
        })
    }
}

impl RoomConfig {
    /// The page's config for proposal `doc` of the open project, a room of its own whose
    /// socket is `<base>/room/<project>/<doc>`.
    pub fn for_proposal(&self, doc: &str, role: Role) -> Option<Self> {
        let mut url = Url::parse(&self.ws_url).ok()?;
        url.path_segments_mut()
            .ok()?
            .pop()
            .push(&self.room_id)
            .push(doc);
        Some(Self {
            room_id: format!("{}/{doc}", self.room_id),
            role,
            ws_url: url.into(),
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

/// Picking `target` while `open` is open changes nothing, unless the page shows the public
/// catalogues' pages: then it goes back to the project.
pub fn stays(open: &str, target: &str, on_public_pages: bool) -> bool {
    open == target && !on_public_pages
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

/// Whether the project can gain people: not a public catalogue (nobody is a member) and not
/// the visitor's own project. A project the list does not hold yet is taken as shareable;
/// the server refuses what it must.
pub fn shareable(room_id: &str, listed: &[Project]) -> bool {
    !cocat_core::room_id::is_public_id(room_id)
        && !listed
            .iter()
            .any(|project| project.id == room_id && project.personal)
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
    use cocat_core::room_id::PUBLIC_PREFIX;

    fn config() -> RoomConfig {
        RoomConfig {
            room_id: "lab".into(),
            room_name: "Solitary Waves".into(),
            role: Role::Owner,
            ws_url: "wss://cocat.example.org/cocat-dev/room/lab".into(),
            api_url: "https://cocat.example.org/cocat-dev/api".into(),
            user_email: "alice@example.org".into(),
            admin: true,
            files: false,
            file_max: None,
        }
    }

    fn project(id: &str, name: &str, role: Role) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            role,
            editors_can_share: true,
            personal: false,
            files: false,
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
    fn editors_rename_and_leave() {
        assert_eq!(
            Actions::of(Role::Editor),
            Actions {
                rename: true,
                leave: true,
                delete: false
            }
        );
    }

    #[test]
    fn viewers_can_only_leave() {
        assert_eq!(
            Actions::of(Role::Viewer),
            Actions {
                rename: false,
                leave: true,
                delete: false
            }
        );
    }

    #[test]
    fn a_public_catalogue_is_renamed_and_deleted_by_curators_and_never_left() {
        assert_eq!(
            Actions::of_public(Role::Editor),
            Actions {
                rename: true,
                leave: false,
                delete: true
            }
        );
        assert_eq!(
            Actions::of_public(Role::Viewer),
            Actions {
                rename: false,
                leave: false,
                delete: false
            }
        );
    }

    #[test]
    fn a_project_without_the_word_has_no_files() {
        let listed: Vec<Project> =
            serde_json::from_str(r#"[{"id":"a","name":"A","role":"owner"}]"#).unwrap();
        assert!(!listed[0].files);
    }

    #[test]
    fn a_project_with_files_opens_with_them() {
        let listed: Vec<Project> = serde_json::from_str(
            r#"[{"id":"w","name":"Notebooks","role":"editor","files":true,"kind":"workspace"}]"#,
        )
        .unwrap();
        assert!(listed[0].files);
        assert!(config().for_project(&listed[0]).unwrap().files);
    }

    #[test]
    fn the_share_button_is_for_projects_that_may_gain_people() {
        let listed: Vec<Project> = serde_json::from_str(
            r#"[{"id":"mine","name":"My catalogues","role":"owner","personal":true},
                {"id":"lab","name":"Lab","role":"owner"}]"#,
        )
        .unwrap();
        assert!(
            !shareable("mine", &listed),
            "the own project stays personal"
        );
        assert!(shareable("lab", &listed));
        assert!(shareable("not-listed-yet", &listed));
        assert!(!shareable(&format!("{PUBLIC_PREFIX}waves"), &listed));
    }

    #[test]
    fn the_list_says_whether_editors_share() {
        let listed: Vec<Project> = serde_json::from_str(
            r#"[{"id":"a","name":"A","role":"editor","editors_can_share":false},
                {"id":"b","name":"B","role":"owner"}]"#,
        )
        .unwrap();
        assert!(!listed[0].editors_can_share);
        assert!(
            listed[1].editors_can_share,
            "a created project lets editors share"
        );
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
                admin: true,
                files: false,
                file_max: None,
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
    fn a_proposal_room_is_a_document_of_the_project_with_its_own_socket() {
        let proposal = config().for_proposal("doc1", Role::Viewer).unwrap();
        assert_eq!(proposal.room_id, "lab/doc1");
        assert_eq!(proposal.role, Role::Viewer);
        assert_eq!(
            proposal.ws_url,
            "wss://cocat.example.org/cocat-dev/room/lab/doc1"
        );
        assert_eq!(proposal.api_url, config().api_url);
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
    fn picking_the_open_project_stays_unless_the_page_shows_the_public_catalogues() {
        assert!(stays("lab", "lab", false));
        assert!(!stays("lab", "lab", true), "the public index leaves for it");
        assert!(!stays("lab", "bepi", false));
        assert!(!stays("public.a", "public.a", true));
    }

    #[test]
    fn going_back_to_a_public_catalogue_opens_it_without_a_reload() {
        let listed = [
            project("lab", "Solitary Waves", Role::Owner),
            project("public.a", "Shocks", Role::Viewer),
        ];
        assert_eq!(
            arrival("lab", "/ui/rooms/public.a/browse", &listed),
            Arrival::Open(listed[1].clone())
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
