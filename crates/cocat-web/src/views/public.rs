//! The public catalogues, a place of their own: their header, a sidebar with the folder tree
//! and the submissions, and the index of one folder's catalogues. A catalogue opens like any
//! project.

use leptos::prelude::*;

use crate::help;
use crate::history::browse_href;
use crate::hits::Hits;
use crate::public::{
    Folder, PublicCatalogue, Shelf, folder_label, folder_tree, index, open_folders,
};
use crate::route::Route;
use crate::views::controls::{Error, Hint, error_line};
use crate::views::header::PublicHeader;
use crate::views::projects::{Projects, set_title};
use crate::views::sidebar::{PublicIcon, keep_current_in_view};
use crate::views::submissions::{
    CopyForm, MySubmissionsPage, SubmissionPage, SubmissionsPage, waiting_submissions,
};
use crate::views::table::marked;

/// The index and the submissions pages. No project is open here, so no connection either.
#[component]
pub fn PublicSpace() -> impl IntoView {
    let projects = expect_context::<Projects>();
    set_title("Public catalogues");
    let error: Error = RwSignal::new(None);
    projects.reload_public(move |failure| {
        error.try_set(Some(failure));
    });
    let shown = Memo::new(move |_| projects.route.get());
    let page = move || match shown.get() {
        Route::Submissions => view! { <SubmissionsPage/> }.into_any(),
        Route::MySubmissions => view! { <MySubmissionsPage/> }.into_any(),
        Route::Submission(id) => view! { <SubmissionPage id/> }.into_any(),
        Route::Public(folder) => view! { <PublicPage folder error/> }.into_any(),
        _ => ().into_any(),
    };
    view! {
        <PublicHeader shown/>
        <div class="browser">
            <PublicSidebar current=None/>
            <crate::views::resize::Resizer side=crate::layout::Side::Sidebar label="Width of the folder list"/>
            <div class="pane">{page}</div>
        </div>
    }
}

/// The index entry of the public catalogue `id`, once the index is read.
pub fn listed(id: String) -> Memo<Option<PublicCatalogue>> {
    let projects = expect_context::<Projects>();
    Memo::new(move |_| {
        projects.public.with(|listed| {
            listed
                .iter()
                .flatten()
                .find(|catalogue| catalogue.id == id)
                .cloned()
        })
    })
}

/// Where the public catalogue `id` comes from, once the index is read; `None` for one people
/// keep.
pub fn relay_of(id: String) -> Memo<Option<crate::public::Relay>> {
    let listed = listed(id);
    Memo::new(move |_| listed.get().and_then(|listed| listed.relay))
}

/// A catalogue's description, folded to a few lines; a button unfolds it when it holds
/// more than those lines show. Its code, links and citation are shown as such (see
/// [`crate::prose`]).
// simplify: whether it overflows is measured once, when it is shown; a window resized
// afterwards keeps the button (or its absence). Measure again on resize if that matters.
#[component]
pub fn Description(
    text: String,
    /// A search whose words are marked in the text.
    #[prop(optional)]
    words: String,
) -> impl IntoView {
    let hits = Hits::words(&words);
    let body = NodeRef::<leptos::html::Div>::new();
    let long = RwSignal::new(false);
    let open = RwSignal::new(false);
    Effect::new(move |_| {
        if let Some(body) = body.get() {
            long.set(body.scroll_height() > body.client_height());
        }
    });
    let paragraphs = crate::prose::paragraphs(&text)
        .into_iter()
        .map(|paragraph| {
            let label = paragraph.cite.then(|| view! { <span class="cite-label">"Cite"</span> " " });
            view! { <p class:cite=paragraph.cite>{label}{paragraph.spans.into_iter().map(|piece| span(piece, &hits)).collect_view()}</p> }
        })
        .collect_view();
    view! {
        <div class="catalogue-description" class:folded=move || !open.get()>
            <div class="prose" node_ref=body>{paragraphs}</div>
            <Show when=move || long.get()>
                <button type="button" class="link-button" aria-expanded=move || open.get().to_string()
                    on:click=move |_| open.update(|open| *open = !*open)>
                    {move || if open.get() { "Show less" } else { "Show all" }}
                </button>
            </Show>
        </div>
    }
}

fn span(span: crate::prose::Span, hits: &Hits) -> AnyView {
    use crate::prose::Span;
    match span {
        Span::Text(text) => marked(hits, DESCRIPTION, &text),
        Span::Code(code) => view! { <code>{marked(hits, DESCRIPTION, &code)}</code> }.into_any(),
        Span::Link { href, text } => {
            let text = marked(hits, DESCRIPTION, &text);
            view! { <a href=href target="_blank" rel="noopener">{text}</a> }.into_any()
        }
    }
}

/// The column name `Hits::words` marks any text under.
const DESCRIPTION: &str = "description";

/// "From DONKI (NASA CCMC), refreshed 2026-11-11", with a link to the list at its source.
pub fn relay_line(relay: &crate::public::Relay) -> impl IntoView + use<> {
    let link = relay.url.clone().map(|url| {
        view! { " · " <a href=url target="_blank" rel="noopener">"source"</a> }
    });
    view! { <span class="relay-line">{relay.text()}{link}</span> }
}

/// The folder tree, "All" above it, then the submissions. Inside a public catalogue
/// (`current`, its id) the tree also lists the catalogues, with that one's folder open.
#[component]
pub fn PublicSidebar(current: Option<String>) -> impl IntoView {
    let projects = expect_context::<Projects>();
    if projects.public.with_untracked(Option::is_none) {
        projects.reload_public(|_| {});
    }
    let page = window().location().pathname().unwrap_or_default();
    let selected = Memo::new(move |_| match projects.route.get() {
        Route::Public(folder) => Some(folder),
        _ => None,
    });
    let unfolded = RwSignal::new(false);
    // On a phone the tree folds away once a folder is picked, so the list shows.
    Effect::new(move |_| {
        selected.track();
        unfolded.set(false);
    });
    let all_href = Route::Public(None).href(&page);
    let total = move || projects.public.with(|all| all.as_ref().map(Vec::len));
    let tree_page = page.clone();
    let tree = move || {
        let page = tree_page.clone();
        let current = current.clone();
        projects.public.with(|all| {
            let all = all.as_deref().unwrap_or_default();
            let folder = selected.get().flatten();
            let tree = Tree::new(all, page, folder, current);
            let folders: Vec<AnyView> = folder_tree(all)
                .into_iter()
                .map(|folder| tree.folder(folder))
                .collect();
            (folders, tree.leaves(None))
        })
    };
    let folders = NodeRef::<leptos::html::Nav>::new();
    keep_current_in_view(folders, move || {
        projects.public.track();
        selected.track();
    });
    view! {
        <aside class="sidebar public-sidebar" aria-label="Public catalogues">
            <button type="button" class="folders-toggle" aria-expanded=move || unfolded.get().to_string()
                on:click=move |_| unfolded.update(|open| *open = !*open)>
                "Folders"<span class="caret" aria-hidden="true">"▾"</span>
            </button>
            <nav class="folder-tree" class:shut=move || !unfolded.get() aria-label="Folders" node_ref=folders>
                <a class="catalogue" href=all_href aria-current=move || current_page(selected.get() == Some(None))>
                    <span class="name"><PublicIcon size=14/><span class="label">"All"</span></span>
                    <span class="count">{total}</span>
                </a>
                {tree}
            </nav>
            <SubmissionEntries page/>
        </aside>
    }
}

/// What the tree needs to draw one folder.
#[derive(Clone)]
struct Tree {
    page: String,
    /// The folder the index shows.
    selected: Option<String>,
    open: Vec<String>,
    /// Inside a public catalogue: every catalogue, and that one's id.
    catalogues: Option<(Vec<PublicCatalogue>, String)>,
}

impl Tree {
    fn new(
        all: &[PublicCatalogue],
        page: String,
        selected: Option<String>,
        current: Option<String>,
    ) -> Self {
        let here = current
            .as_deref()
            .and_then(|id| all.iter().find(|catalogue| catalogue.id == id))
            .and_then(PublicCatalogue::folder_path);
        let open = here
            .or_else(|| selected.clone())
            .map(|path| open_folders(&path))
            .unwrap_or_default();
        Self {
            page,
            selected,
            open,
            catalogues: current.map(|id| (all.to_vec(), id)),
        }
    }

    fn folder(&self, folder: Folder) -> AnyView {
        let href = Route::Public(Some(folder.path.clone())).href(&self.page);
        let here = self.selected.as_deref() == Some(folder.path.as_str());
        let leaves = self.leaves(Some(&folder.path));
        let open = self.open.contains(&folder.path);
        let link = view! {
            <a class="catalogue" href=href aria-current=current_page(here)>
                <span class="name"><span class="label">{folder.name}</span></span>
                <span class="count">{folder.count}</span>
            </a>
        };
        if folder.children.is_empty() && leaves.is_empty() {
            return view! { <div class="folder-node">{link}</div> }.into_any();
        }
        let children: Vec<AnyView> = folder
            .children
            .into_iter()
            .map(|child| self.folder(child))
            .collect();
        view! {
            <details class="folder-node" open=open>
                <summary><span class="twisty" aria-hidden="true"></span>{link}</summary>
                <div class="folder-children">{children}{leaves}</div>
            </details>
        }
        .into_any()
    }

    /// The catalogues filed right in `folder` (`None`: filed nowhere), inside a catalogue only.
    fn leaves(&self, folder: Option<&str>) -> Vec<AnyView> {
        let Some((all, current)) = &self.catalogues else {
            return Vec::new();
        };
        all.iter()
            .filter(|catalogue| catalogue.folder_path().as_deref() == folder)
            .map(|catalogue| leaf(catalogue, catalogue.id == *current))
            .collect()
    }
}

fn leaf(catalogue: &PublicCatalogue, here: bool) -> AnyView {
    let projects = expect_context::<Projects>();
    let project = catalogue.project();
    view! {
        <a class="catalogue leaf" href=browse_href(&catalogue.id) aria-current=current_page(here)
            on:click=move |event| { event.prevent_default(); projects.switch(&project); }>
            <span class="name"><span class="label">{catalogue.name.clone()}</span></span>
        </a>
    }
    .into_any()
}

/// The curators' queue, with how many wait, and the visitor's own submissions.
#[component]
fn SubmissionEntries(page: String) -> impl IntoView {
    let projects = expect_context::<Projects>();
    let waiting = waiting_submissions(projects.api());
    let shown = projects.route;
    let queue = Route::Submissions.href(&page);
    let mine = Route::MySubmissions.href(&page);
    view! {
        <div class="public-more">
            {move || waiting.get().map(|count| view! {
                <a class="catalogue" href=queue.clone()
                    aria-current=move || current_page(matches!(shown.get(), Route::Submissions | Route::Submission(_)))>
                    <span class="name"><span class="label">"Submissions"</span></span>
                    {(count > 0).then(|| view! {
                        <span class="count-badge" aria-label=format!("{count} waiting")>{count}</span>
                    })}
                </a>
            })}
            <a class="catalogue" href=mine aria-current=move || current_page(shown.get() == Route::MySubmissions)>
                <span class="name"><span class="label">"My submissions"</span></span>
            </a>
        </div>
    }
}

fn current_page(here: bool) -> Option<&'static str> {
    here.then_some("page")
}

/// One folder's catalogues (all of them for `None`), narrowed by a search within it.
#[component]
fn PublicPage(folder: Option<String>, error: Error) -> impl IntoView {
    let projects = expect_context::<Projects>();
    let all = projects.public;
    let search = RwSignal::new(String::new());
    let label = folder.as_deref().map(folder_label);
    let placeholder = label.as_ref().map_or_else(
        || "Search all public catalogues".to_owned(),
        |label| format!("Search in {label}"),
    );
    let title = label.unwrap_or_else(|| "All public catalogues".to_owned());
    let none_yet = move || all.with(|all| all.as_ref().is_some_and(Vec::is_empty));
    let here = folder.clone();
    let shown = move || {
        all.with(|all| {
            all.as_ref().map(|all| {
                let shelves = search.with(|search| index(all, folder.as_deref(), search));
                (all.is_empty(), shelves)
            })
        })
    };
    view! {
        <main class="content groups-page public-page">
            <div class="groups-head">
                <h1>{title}</h1>
            </div>
            <Hint text="Catalogues everyone signed in can read. Curators keep them." anchor="public" topic="public catalogues"/>
            <input type="search" class="public-search" hidden=none_yet placeholder=placeholder.clone()
                aria-label=placeholder
                prop:value=move || search.get() on:input=move |event| search.set(event_target_value(&event))/>
            {error_line(error)}
            {move || shown().map(|(empty, shelves)| match (empty, shelves.is_empty()) {
                (true, _) => view! { <NoneYet/> }.into_any(),
                (false, true) => view! { <p class="quiet">"No public catalogue matches."</p> }.into_any(),
                (false, false) => shelves.into_iter().map(|shelf| {
                    let titled = shelf.folder != here;
                    let words = search.get_untracked();
                    view! { <PublicShelf shelf titled words/> }
                }).collect_view().into_any(),
            })}
        </main>
    }
}

/// What the index says before the first public catalogue: who adds them, and how to offer one.
#[component]
fn NoneYet() -> impl IntoView {
    view! {
        <section class="empty-state" aria-labelledby="none-yet">
            <span class="empty-icon"><PublicIcon size=28/></span>
            <h2 id="none-yet">"No public catalogue yet"</h2>
            <p>"Curators add them. They import shared catalogues, such as AMDA's, and accept what people submit."</p>
            <p>"Anyone can offer one: open a catalogue in one of your projects and choose "
                <strong>"More › Submit to Public catalogues…"</strong>" above its events."</p>
            <a class="button" href=help::href("public") target="_blank" rel="noopener">"How public catalogues work"</a>
        </section>
    }
}

/// One folder's catalogues; `titled` is false for the folder the page is about, whose name
/// is already the page's title.
#[component]
fn PublicShelf(shelf: Shelf, titled: bool, words: String) -> impl IntoView {
    let label = marked(&Hits::words(&words), "folder", &shelf.label());
    let count = shelf.catalogues.len();
    let list = view! {
        <ul class="plain-list">
            {shelf.catalogues.into_iter().map(|catalogue| view! { <PublicRow catalogue words=words.clone()/> }).collect_view()}
        </ul>
    };
    match titled {
        true => view! {
            <details class="public-shelf" open>
                <summary><span class="label">{label}</span><small>{count}</small></summary>
                {list}
            </details>
        }
        .into_any(),
        false => list.into_any(),
    }
}

#[component]
/// `words`: the page's search, marked in what the row shows of the catalogue.
fn PublicRow(catalogue: PublicCatalogue, words: String) -> impl IntoView {
    let hits = Hits::words(&words);
    let mark = |text: &str| marked(&hits, "text", text);
    let projects = expect_context::<Projects>();
    let project = catalogue.project();
    let day = catalogue.day().map(|day| format!("changed {day}"));
    let copying = RwSignal::new(false);
    let close = Callback::new(move |()| copying.set(false));
    let public = catalogue.id.clone();
    view! {
        <li class="public-item">
            <div class="public-row">
                <button type="button" class="public-open name" on:click=move |_| projects.switch(&project)>
                    {mark(&catalogue.name)}
                </button>
                {catalogue.description.clone().map(|text| view! { <Description text words=words.clone()/> })}
                <span class="meta">
                    {match catalogue.relay.as_ref() {
                        Some(relay) => view! { <span class="source">{mark(&relay.text())}</span> }.into_any(),
                        None => catalogue.source.as_deref().map(|source| view! { <span class="source">{mark(source)}</span> }).into_any(),
                    }}
                    {catalogue.events().map(|events| view! { <span>{events}</span> })}
                    {day.map(|day| view! { <span>{day}</span> })}
                </span>
            </div>
            <button type="button" class="button copy-button" aria-expanded=move || copying.get().to_string()
                aria-label=format!("Copy “{}” to a project", catalogue.name)
                on:click=move |_| copying.update(|open| *open = !*open)>"Copy…"</button>
            <Show when=move || copying.get()>
                <CopyForm public=public.clone() close/>
            </Show>
        </li>
    }
}
