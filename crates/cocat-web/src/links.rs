//! Links to catalogues of other projects: whether the visitor can open one, and what the
//! picker offers.

use cocat_core::model::Link;
use cocat_core::room_id::is_public_id;
use serde::Deserialize;

use crate::projects::Project;

/// A catalogue as another project's `GET /api/rooms/{id}/catalogues` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Listed {
    pub uuid: String,
    pub name: String,
    #[serde(default)]
    pub deleted: bool,
}

#[derive(Debug, Deserialize)]
pub struct ListedCatalogues {
    pub catalogues: Vec<Listed>,
}

/// Why another project's catalogues could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unread {
    /// The server said no: the visitor is not in the project any more.
    Refused,
    /// No answer: the network or the server is down for now.
    Unreachable,
}

/// What reading another project's catalogues gave: `None` while it is on its way.
pub type Lookup<'a> = Option<&'a Result<Vec<Listed>, Unread>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// Not known yet: the projects or the original's catalogues are still on their way.
    Loading,
    /// The visitor is in the original project and its catalogue is there, named `name` now.
    Open { project: Project, name: String },
    /// Not in the original project any more, or the catalogue was deleted.
    Gone,
    /// The original's catalogues could not be read for now.
    Unreachable,
}

/// Whether `link` opens, from the visitor's `projects` (empty until read) and the original
/// project's catalogues.
pub fn reach(link: &Link, projects: &[Project], catalogues: Lookup<'_>) -> Reach {
    if projects.is_empty() {
        return Reach::Loading;
    }
    let Some(project) = projects.iter().find(|project| project.id == link.project) else {
        return Reach::Gone;
    };
    let Some(listed) = catalogues else {
        return Reach::Loading;
    };
    let catalogues = match listed {
        Ok(catalogues) => catalogues,
        Err(Unread::Refused) => return Reach::Gone,
        Err(Unread::Unreachable) => return Reach::Unreachable,
    };
    let found = catalogues
        .iter()
        .find(|catalogue| catalogue.uuid == link.catalogue && !catalogue.deleted);
    match found {
        Some(catalogue) => Reach::Open {
            project: project.clone(),
            name: catalogue.name.clone(),
        },
        None => Reach::Gone,
    }
}

/// The name a link shows: the original's current one once it opens, else the one it had.
pub fn shown_name(link: &Link, reach: &Reach) -> String {
    match reach {
        Reach::Open { name, .. } => name.clone(),
        _ => link.name.clone(),
    }
}

/// The original project's name: as the visitor's list has it, else as it was when linked.
pub fn project_name(link: &Link, projects: &[Project]) -> String {
    let listed = projects.iter().find(|project| project.id == link.project);
    match listed {
        Some(project) => project.name.clone(),
        None if link.project_name.is_empty() => "another project".to_owned(),
        None => link.project_name.clone(),
    }
}

/// The projects a link can point into: the visitor's others, not the public catalogues.
pub fn sources(projects: &[Project], open: &str) -> Vec<Project> {
    projects
        .iter()
        .filter(|project| project.id != open && !is_public_id(&project.id))
        .cloned()
        .collect()
}

/// What the picker offers from `project`: its catalogues not deleted and not linked yet,
/// by name.
pub fn linkable(catalogues: &[Listed], links: &[Link], project: &str) -> Vec<Listed> {
    let linked = |uuid: &str| {
        links
            .iter()
            .any(|link| link.project == project && link.catalogue == uuid)
    };
    let mut offered: Vec<Listed> = catalogues
        .iter()
        .filter(|catalogue| !catalogue.deleted && !linked(&catalogue.uuid))
        .cloned()
        .collect();
    offered.sort_by_cached_key(|catalogue| catalogue.name.to_lowercase());
    offered
}

#[cfg(test)]
mod tests {
    use cocat_core::role::Role;

    use super::*;

    fn project(id: &str, name: &str) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            role: Role::Editor,
            editors_can_share: true,
            personal: false,
            files: false,
        }
    }

    fn link(project: &str, catalogue: &str) -> Link {
        Link {
            id: "l1".into(),
            project: project.into(),
            project_name: "Lab then".into(),
            catalogue: catalogue.into(),
            name: "Shocks then".into(),
            by: None,
            at: 0,
        }
    }

    fn listed(uuid: &str, name: &str, deleted: bool) -> Listed {
        Listed {
            uuid: uuid.into(),
            name: name.into(),
            deleted,
        }
    }

    #[test]
    fn a_link_opens_with_the_originals_current_name() {
        let projects = [project("mine", "Mine"), project("lab", "Lab")];
        let catalogues = Ok(vec![listed("c1", "Shocks now", false)]);
        let reach = reach(&link("lab", "c1"), &projects, Some(&catalogues));
        assert_eq!(
            reach,
            Reach::Open {
                project: project("lab", "Lab"),
                name: "Shocks now".into()
            }
        );
        assert_eq!(shown_name(&link("lab", "c1"), &reach), "Shocks now");
    }

    #[test]
    fn a_link_waits_for_the_projects_and_the_catalogues() {
        let catalogues = Ok(vec![listed("c1", "Shocks", false)]);
        assert_eq!(
            reach(&link("lab", "c1"), &[], Some(&catalogues)),
            Reach::Loading
        );
        let projects = [project("lab", "Lab")];
        assert_eq!(reach(&link("lab", "c1"), &projects, None), Reach::Loading);
        assert_eq!(
            shown_name(&link("lab", "c1"), &Reach::Loading),
            "Shocks then"
        );
    }

    #[test]
    fn a_link_into_a_project_the_visitor_left_is_gone() {
        let projects = [project("mine", "Mine")];
        assert_eq!(reach(&link("lab", "c1"), &projects, None), Reach::Gone);
        assert_eq!(project_name(&link("lab", "c1"), &projects), "Lab then");
    }

    #[test]
    fn a_link_to_a_deleted_or_missing_catalogue_is_gone() {
        let projects = [project("lab", "Lab")];
        let deleted = Ok(vec![listed("c1", "Shocks", true)]);
        assert_eq!(
            reach(&link("lab", "c1"), &projects, Some(&deleted)),
            Reach::Gone
        );
        let other = Ok(vec![listed("c2", "Other", false)]);
        assert_eq!(
            reach(&link("lab", "c1"), &projects, Some(&other)),
            Reach::Gone
        );
        assert_eq!(
            reach(&link("lab", "c1"), &projects, Some(&Err(Unread::Refused))),
            Reach::Gone
        );
    }

    #[test]
    fn a_link_whose_project_cannot_be_reached_is_not_gone() {
        let projects = [project("lab", "Lab")];
        let unreachable = Err(Unread::Unreachable);
        assert_eq!(
            reach(&link("lab", "c1"), &projects, Some(&unreachable)),
            Reach::Unreachable
        );
    }

    #[test]
    fn the_project_is_named_as_the_visitor_knows_it() {
        let projects = [project("lab", "Lab renamed")];
        assert_eq!(project_name(&link("lab", "c"), &projects), "Lab renamed");
        let mut older = link("gone", "c");
        older.project_name.clear();
        assert_eq!(project_name(&older, &projects), "another project");
    }

    #[test]
    fn links_point_into_the_visitors_other_projects() {
        let public = format!("{}x", cocat_core::room_id::PUBLIC_PREFIX);
        let projects = [
            project("mine", "Mine"),
            project("lab", "Lab"),
            project(&public, "AMDA"),
        ];
        let ids: Vec<String> = sources(&projects, "mine")
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, ["lab"]);
    }

    #[test]
    fn the_picker_offers_what_is_not_linked_yet() {
        let catalogues = [
            listed("c1", "beta", false),
            listed("c2", "Alpha", false),
            listed("c3", "gone", true),
            listed("c4", "linked", false),
        ];
        let links = [link("lab", "c4"), link("other", "c2")];
        let names: Vec<String> = linkable(&catalogues, &links, "lab")
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["Alpha", "beta"]);
    }

    #[test]
    fn a_catalogue_list_reads_from_the_api() {
        let body = r#"{"catalogues":[{"uuid":"c1","name":"Shocks","deleted":false,"tags":[],"event_count":3}]}"#;
        let read: ListedCatalogues = serde_json::from_str(body).unwrap();
        assert_eq!(read.catalogues, [listed("c1", "Shocks", false)]);
    }
}
