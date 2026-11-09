//! Around the filter box: what is wrong with the search, saving it as a smart catalogue or
//! into the open one, and the builder that writes a search from rows.

use cocat_core::model;
use cocat_core::query::{self, FIELDS, Kind};
use leptos::prelude::*;

use crate::edit;
use crate::route::Route;
use crate::search::{
    Choice, Completion, Piece, Query, Saved, Vocabulary, accept, choices, clause, complete, label,
    needs_value, pieces, search_text,
};
use crate::state::Room;
use crate::views::controls::{Error, commit, error_line, go_to};

/// What the shown catalogue's rule reads as, when it is a smart catalogue with a search.
pub fn saved_search(room: &Room, shown: &Route) -> Option<Saved> {
    let Route::Catalogue(uuid) = shown else {
        return None;
    };
    room.read(|txn| {
        let rule = model::catalogue(txn, uuid)?.rule?;
        let names = model::catalogue_names(txn);
        let (text, source) = query::rule_query(&rule, &names)?;
        Some(Saved { text, source })
    })
}

/// The line under the box: the search's problem, and the buttons that save it.
#[component]
pub fn SearchLine(
    shown: Memo<Route>,
    query: RwSignal<String>,
    parsed: Memo<Query>,
    saved: Memo<Option<Saved>>,
    scope: RwSignal<Option<String>>,
    home: Memo<Option<(String, String)>>,
) -> impl IntoView {
    let room = expect_context::<Room>();
    let can_write = room.can_write();
    let saving = RwSignal::new(false);
    let error: Error = RwSignal::new(None);
    let readable = move || parsed.with(|parsed| matches!(parsed, Query::Words | Query::Rule(_)));
    let edited = move || {
        saved.with(|saved| {
            saved.as_ref().is_some_and(|saved| {
                query.with(|text| text.trim() != saved.text)
                    || scope.with(|scope| *scope != saved.source)
            })
        })
    };
    let smart = move || saved.with(Option::is_some);
    let offer_save = move || can_write && !smart() && readable();
    let writer = room.clone();
    let save_changes = move |()| {
        let Route::Catalogue(uuid) = shown.get_untracked() else {
            return;
        };
        let text = query.get_untracked();
        let source = scope.get_untracked();
        commit(&writer, error, |txn| {
            let names = model::catalogue_names(txn);
            let predicate = query::parse(&text, &names)?;
            edit::replace_rule(txn, &uuid, &query::rule(&predicate, source.as_deref()))
        });
    };
    let save_changes = StoredValue::new_local(save_changes);
    let revert = move |_| {
        if let Some(saved) = saved.get_untracked() {
            query.set(saved.text);
            scope.set(saved.source);
        }
    };
    view! {
        {move || parsed.with(|parsed| match parsed {
            Query::Broken(why) => Some(view! { <p class="field-error search-error" role="alert">{why.clone()}</p> }),
            _ => None,
        })}
        <div class="search-line">
            <ScopeSwitch scope home/>
            <Show when=move || edited()>
                <span class="quiet">"The table shows what this search would hold."</span>
                <Show when=move || can_write && readable()>
                    <button type="button" class="button primary" on:click=move |_| save_changes.with_value(|save| save(()))>"Save changes"</button>
                </Show>
                <button type="button" class="button" on:click=revert>"Revert"</button>
            </Show>
            <Show when=move || offer_save() && !saving.get()>
                <button type="button" class="button" on:click=move |_| saving.set(true)>"Save as smart catalogue…"</button>
            </Show>
        </div>
        <Show when=move || saving.get()>
            <SaveSearch query scope saving/>
        </Show>
        {error_line(error)}
    }
}

/// Names the new smart catalogue: it searches where the table does.
#[component]
fn SaveSearch(
    query: RwSignal<String>,
    scope: RwSignal<Option<String>>,
    saving: RwSignal<bool>,
) -> impl IntoView {
    let room = expect_context::<Room>();
    let name = RwSignal::new(String::new());
    let error: Error = RwSignal::new(None);
    let reader = room.clone();
    let keeps = move || match scope.get() {
        Some(uuid) => {
            let name = reader
                .read(|txn| model::catalogue(txn, &uuid))
                .map(|catalogue| catalogue.name)
                .unwrap_or_default();
            format!(
                "It keeps the events of “{name}” that match the search, as people edit the project."
            )
        }
        None => "It keeps every event of the project that matches the search, as people edit it."
            .to_owned(),
    };
    let create = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let text = query.get_untracked();
        let name = name.get_untracked();
        let source = scope.get_untracked();
        let author = room.config.user_email.clone();
        let made = room.write(|txn| {
            let names = model::catalogue_names(txn);
            let predicate = query::parse(&text, &names)?;
            let rule = query::rule(&predicate, source.as_deref());
            edit::new_smart_catalogue(txn, &name, &author, &rule)
        });
        match made {
            Ok(uuid) => {
                saving.set(false);
                go_to(&Route::Catalogue(uuid).hash());
            }
            Err(failure) => error.set(Some(failure.to_string())),
        }
    };
    view! {
        <form class="inline-form search-save" on:submit=create>
            <label class="field">
                <span class="field-label">"Name of the smart catalogue"</span>
                <input type="text" required prop:value=move || name.get()
                    on:input=move |event| name.set(event_target_value(&event))/>
            </label>
            <p class="quiet">{keeps}</p>
            {error_line(error)}
            <div class="row-buttons">
                <button type="submit" class="button primary">"Create"</button>
                <button type="button" class="button" on:click=move |_| saving.set(false)>"Cancel"</button>
            </div>
        </form>
    }
}

/// Where the search looks: one catalogue, or every event of the project.
#[component]
pub fn ScopeSwitch(
    scope: RwSignal<Option<String>>,
    home: Memo<Option<(String, String)>>,
) -> impl IntoView {
    move || {
        home.get().map(|(uuid, name)| {
            let here = uuid.clone();
            let inside = move || scope.with(|scope| scope.as_deref() == Some(here.as_str()));
            let outside = move || scope.with(Option::is_none);
            view! {
                <div class="scope" role="group" aria-label="Search in">
                    <span class="quiet">"Search in"</span>
                    <button type="button" class="scope-option" aria-pressed=move || inside().to_string()
                        on:click=move |_| scope.set(Some(uuid.clone()))>{format!("“{name}”")}</button>
                    <button type="button" class="scope-option" aria-pressed=move || outside().to_string()
                        on:click=move |_| scope.set(None)>"All events"</button>
                </div>
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Condition {
    id: usize,
    field: String,
    choice: Choice,
    value: String,
}

impl Condition {
    fn new(id: usize) -> Self {
        let field = "tag".to_owned();
        Self {
            id,
            choice: choices(kind(&field))[0],
            field,
            value: String::new(),
        }
    }

    fn text(&self) -> String {
        let complete = !needs_value(self.choice) || !self.value.trim().is_empty();
        match complete {
            true => clause(&self.field, self.choice, &self.value),
            false => String::new(),
        }
    }
}

fn kind(field: &str) -> Kind {
    query::kind_of(field).unwrap_or(Kind::Text)
}

/// How a field reads in the builder's menu.
fn field_label(field: &str) -> String {
    match field {
        "start" => "Start".into(),
        "stop" => "Stop".into(),
        "duration" => "Duration".into(),
        "tag" => "Tag".into(),
        "status" => "Status".into(),
        "confidence" => "Confidence".into(),
        "author" => "Author".into(),
        "catalogue" => "Catalogue".into(),
        "created" => "Added".into(),
        "origin" => "Made by".into(),
        "source_id" => "Source id".into(),
        "product" => "Product".into(),
        "start_err" => "Start uncertainty (s)".into(),
        "stop_err" => "Stop uncertainty (s)".into(),
        "uuid" => "Event id".into(),
        "shape" => "Shape".into(),
        other => match other.strip_prefix('#') {
            Some(key) => format!("{key} (attribute)"),
            None => other.into(),
        },
    }
}

/// Rows of field, test and value that write a search into the box.
#[component]
pub fn SearchBuilder(
    query: RwSignal<String>,
    building: RwSignal<bool>,
    keys: Memo<Vec<String>>,
) -> impl IntoView {
    let room = expect_context::<Room>();
    let catalogues = room.read_untracked(|txn| model::catalogue_names(txn));
    let next = StoredValue::new(1usize);
    let conditions = RwSignal::new(vec![Condition::new(0)]);
    let any = RwSignal::new(false);
    let fields = move || {
        let fixed = FIELDS.iter().map(|field| field.name.to_owned());
        let attributes = keys.get().into_iter().map(|key| format!("#{key}"));
        fixed.chain(attributes).collect::<Vec<_>>()
    };
    let add = move |_| {
        let id = next.get_value();
        next.set_value(id + 1);
        conditions.update(|all| all.push(Condition::new(id)));
    };
    let apply = move |_| {
        let clauses: Vec<String> = conditions.with(|all| all.iter().map(Condition::text).collect());
        query.set(search_text(&clauses, any.get_untracked()));
        building.set(false);
    };
    let row = move |condition: Condition| {
        let id = condition.id;
        let update = move |change: &dyn Fn(&mut Condition)| {
            conditions.update(|all| {
                if let Some(found) = all.iter_mut().find(|found| found.id == id) {
                    change(found);
                }
            });
        };
        let current =
            move || conditions.with(|all| all.iter().find(|found| found.id == id).cloned());
        let field = move || current().map(|found| found.field).unwrap_or_default();
        let choice = move || current().map_or(Choice::Is, |found| found.choice);
        let value = move || current().map(|found| found.value).unwrap_or_default();
        let catalogues = catalogues.clone();
        view! {
            <div class="condition">
                <select aria-label="Field" on:change=move |event| {
                    let picked = event_target_value(&event);
                    update(&|found| {
                        found.choice = choices(kind(&picked))[0];
                        found.field = picked.clone();
                        found.value.clear();
                    });
                }>
                    {fields().into_iter().map(|name| {
                        let selected = name == field();
                        view! { <option value=name.clone() selected=selected>{field_label(&name)}</option> }
                    }).collect_view()}
                </select>
                {move || {
                    let kind = kind(&field());
                    view! {
                        <select aria-label="Test" on:change=move |event| {
                            let index: usize = event_target_value(&event).parse().unwrap_or(0);
                            update(&|found| found.choice = choices(kind)[index]);
                        }>
                            {choices(kind).iter().enumerate().map(|(index, &option)| view! {
                                <option value=index.to_string() selected=option == choice()>{label(kind, option)}</option>
                            }).collect_view()}
                        </select>
                    }
                }}
                {move || {
                    let catalogues = catalogues.clone();
                    let (kind, choice) = (kind(&field()), choice());
                    let set = move |event| update(&|found| found.value = event_target_value(&event));
                    needs_value(choice).then(|| match (kind, choice) {
                        (Kind::Enum(values), _) => view! {
                            <select aria-label="Value" on:change=set>
                                <option value="">"…"</option>
                                {values.iter().map(|name| view! { <option value=*name selected=*name == value()>{*name}</option> }).collect_view()}
                            </select>
                        }.into_any(),
                        (Kind::Catalogue, _) => view! {
                            <select aria-label="Value" on:change=set>
                                <option value="">"…"</option>
                                {catalogues.into_iter().map(|(_, name)| {
                                    let selected = name == value();
                                    let shown = name.clone();
                                    view! { <option value=name selected=selected>{shown}</option> }
                                }).collect_view()}
                            </select>
                        }.into_any(),
                        (Kind::Time, Choice::InLastDays) => view! {
                            <input type="number" min="1" aria-label="Days" placeholder="days" prop:value=value on:input=set/>
                        }.into_any(),
                        (Kind::Time, _) => view! {
                            <input type="date" aria-label="Date" prop:value=value on:input=set/>
                        }.into_any(),
                        (Kind::Number, _) => view! {
                            <input type="number" step="any" aria-label="Value" prop:value=value on:input=set/>
                        }.into_any(),
                        (Kind::Duration, _) => view! {
                            <input type="text" aria-label="Duration" placeholder="10min, 2h, 30s" prop:value=value on:input=set/>
                        }.into_any(),
                        _ => view! {
                            <input type="text" aria-label="Value" prop:value=value on:input=set/>
                        }.into_any(),
                    })
                }}
                <button type="button" class="link-button remove" aria-label="Remove this condition"
                    on:click=move |_| conditions.update(|all| all.retain(|found| found.id != id))>"Remove"</button>
            </div>
        }
    };
    view! {
        <section class="inline-form search-builder" aria-label="Build a search">
            <p class="builder-head">
                "Events that match "
                <select aria-label="All or any" on:change=move |event| any.set(event_target_value(&event) == "any")>
                    <option value="all" selected=move || !any.get()>"all"</option>
                    <option value="any" selected=move || any.get()>"any"</option>
                </select>
                " of these:"
            </p>
            <For each=move || conditions.get() key=|condition| condition.id children=row/>
            <button type="button" class="link-button" on:click=add>"+ Add a condition"</button>
            <div class="row-buttons">
                <button type="button" class="button primary" on:click=apply>"Use this search"</button>
                <button type="button" class="button" on:click=move |_| building.set(false)>"Cancel"</button>
            </div>
        </section>
    }
}

/// What the project holds that a search can name: read when the completions open.
fn vocabulary(room: &Room) -> Vocabulary {
    room.read(|txn| {
        let has_author = cocat_core::rules::Predicate::Has("author".to_owned());
        let now = cocat_core::time::Timestamp::now();
        let authors: std::collections::BTreeSet<String> = model::search(txn, &has_author, now)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|event| event.author)
            .collect();
        Vocabulary {
            tags: model::existing_tags(txn),
            authors: authors.into_iter().collect(),
            catalogues: model::catalogue_names(txn)
                .into_iter()
                .map(|(_, name)| name)
                .collect(),
            attributes: model::attribute_types(txn).into_keys().collect(),
        }
    })
}

/// The search box: it suggests fields and the project's own values as one types, and holds
/// the button that opens the builder.
#[component]
pub fn SearchBox(
    query: RwSignal<String>,
    building: RwSignal<bool>,
    children: Children,
) -> impl IntoView {
    let room = expect_context::<Room>();
    let input = NodeRef::<leptos::html::Input>::new();
    let open = RwSignal::new(false);
    let cursor = RwSignal::new(0usize);
    let active = RwSignal::new(0usize);
    let words = Memo::new(move |_| open.get().then(|| vocabulary(&room)));
    let completion = Memo::new(move |_| {
        let vocabulary = words.get()?;
        query.with(|text| complete(text, cursor.get(), &vocabulary))
    });
    let caret = move || {
        input
            .get_untracked()
            .and_then(|input| input.selection_start().ok().flatten())
            .map_or(0, |at| at as usize)
    };
    let take = move |index: usize| {
        let Some(found) = completion.get_untracked() else {
            return;
        };
        let Some(chosen) = found.suggestions.get(index) else {
            return;
        };
        let (text, at) = query.with_untracked(|text| accept(text, &found, chosen));
        if let Some(input) = input.get_untracked() {
            input.set_value(&text);
            let _ = input.set_selection_range(at as u32, at as u32);
        }
        query.set(text);
        cursor.set(at);
        active.set(0);
    };
    let keys = move |event: leptos::ev::KeyboardEvent| {
        let count = completion
            .with_untracked(|found| found.as_ref().map_or(0, |found| found.suggestions.len()));
        if count == 0 || !open.get_untracked() {
            return;
        }
        match event.key().as_str() {
            "ArrowDown" => active.update(|at| *at = (*at + 1) % count),
            "ArrowUp" => active.update(|at| *at = (*at + count - 1) % count),
            "Enter" | "Tab" => take(active.get_untracked()),
            "Escape" => open.set(false),
            _ => return,
        }
        event.prevent_default();
    };
    let list = move || {
        let found: Completion = completion.get().filter(|_| open.get())?;
        Some(view! {
            <ul class="completions" id="search-completions" role="listbox" aria-label="Suggestions">
                {found.suggestions.into_iter().enumerate().map(|(index, suggestion)| view! {
                    <li role="option" aria-selected=move || (active.get() == index).to_string()
                        on:mousedown=move |event| { event.prevent_default(); take(index); }
                        on:mouseenter=move |_| active.set(index)>
                        <code>{suggestion.insert.trim_end().to_owned()}</code>
                        <span class="kind">{suggestion.kind}</span>
                    </li>
                }).collect_view()}
            </ul>
        })
    };
    let scroll = RwSignal::new(0i32);
    let follow = move || {
        if let Some(input) = input.get_untracked() {
            scroll.set(input.scroll_left());
        }
    };
    let painted =
        move || query.with(|text| pieces(text).into_iter().map(painted_piece).collect_view());
    view! {
        <label class="search">
            {children()}
            <span class="search-field">
            <span class="search-paint" aria-hidden="true">
                <span class="search-paint-line" style=move || format!("transform: translateX({}px)", -scroll.get())>
                    {painted}
                </span>
            </span>
            <input
                node_ref=input
                type="text"
                on:scroll=move |_| follow()
                role="combobox"
                aria-autocomplete="list"
                aria-controls="search-completions"
                aria-expanded=move || completion.with(|found| (open.get() && found.is_some()).to_string())
                placeholder="Search events"
                aria-label="Search events"
                autocomplete="off"
                spellcheck="false"
                prop:value=move || query.get()
                on:input=move |event| {
                    query.set(event_target_value(&event));
                    cursor.set(caret());
                    active.set(0);
                    open.set(true);
                    follow();
                }
                on:focus=move |_| { cursor.set(caret()); open.set(true); }
                on:blur=move |_| open.set(false)
                on:click=move |_| cursor.set(caret())
                on:keyup=move |event| {
                    if matches!(event.key().as_str(), "ArrowLeft" | "ArrowRight" | "Home" | "End") {
                        cursor.set(caret());
                    }
                    follow();
                }
                on:keydown=keys
            />
            </span>
            <button type="button" class="build-search" aria-label="Build a search"
                title="Build a search" aria-pressed=move || building.get().to_string()
                on:click=move |_| building.update(|open| *open = !*open)>
                <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                    stroke-width="2" aria-hidden="true">
                    <path d="M4 6h10M18 6h2M4 12h4M12 12h8M4 18h12M20 18h0"/>
                    <circle cx="16" cy="6" r="2"/><circle cx="10" cy="12" r="2"/><circle cx="18" cy="18" r="2"/>
                </svg>
            </button>
            {list}
        </label>
    }
}

/// One piece of the search, coloured: a term as a token in its field's colour, keywords and
/// brackets in their own, an unknown field in red.
fn painted_piece(piece: Piece) -> AnyView {
    match piece {
        Piece::Term { field, op, value } => {
            let kind = term_kind(&field);
            view! {
                <span class=format!("term term-{kind}")>
                    <span class="term-field">{field}</span>
                    <span class="term-op">{op}</span>
                    <span class="term-value">{value}</span>
                </span>
            }
            .into_any()
        }
        Piece::Keyword(text) => view! { <span class="keyword">{text}</span> }.into_any(),
        Piece::Bracket(text) => view! { <span class="bracket">{text}</span> }.into_any(),
        Piece::Word(text) => view! { <span class="term term-word">{text}</span> }.into_any(),
        Piece::Space(text) => view! { <span>{text}</span> }.into_any(),
    }
}

fn term_kind(field: &str) -> &'static str {
    match (field, query::kind_of(field)) {
        ("_exists_", _) => "exists",
        (_, None) => "unknown",
        (_, Some(Kind::Tags)) => "tag",
        (_, Some(Kind::Enum(_))) => "status",
        (_, Some(Kind::Catalogue)) => "catalogue",
        (_, Some(Kind::Time | Kind::Duration)) => "time",
        (_, Some(Kind::Number)) => "number",
        (_, Some(Kind::Attribute)) => "attribute",
        _ => "text",
    }
}
