//! Around the filter box: what is wrong with the search, saving it as a smart catalogue or
//! into the open one, and the builder that writes a search from rows.

use cocat_core::model;
use cocat_core::query::{self, FIELDS, Kind};
use leptos::prelude::*;

use crate::edit;
use crate::route::Route;
use crate::search::{Choice, Query, choices, clause, label, needs_value, search_text};
use crate::state::Room;
use crate::views::controls::{Error, commit, error_line, go_to};

/// The shown smart catalogue's search: its text, and the catalogue it searches in (`None`:
/// all events).
#[derive(Debug, Clone, PartialEq)]
pub struct Saved {
    pub text: String,
    pub source: Option<String>,
}

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
) -> impl IntoView {
    let room = expect_context::<Room>();
    let can_write = room.can_write();
    let saving = RwSignal::new(false);
    let error: Error = RwSignal::new(None);
    let readable = move || parsed.with(|parsed| matches!(parsed, Query::Words | Query::Rule(_)));
    let edited = move || {
        saved.with(|saved| {
            saved
                .as_ref()
                .is_some_and(|saved| query.with(|text| text.trim() != saved.text))
        })
    };
    let smart = move || saved.with(Option::is_some);
    let offer_save = move || can_write && !smart() && readable();
    let writer = room.clone();
    let save_changes = move |()| {
        let Some(Saved { source, .. }) = saved.get_untracked() else {
            return;
        };
        let Route::Catalogue(uuid) = shown.get_untracked() else {
            return;
        };
        let text = query.get_untracked();
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
        }
    };
    view! {
        {move || parsed.with(|parsed| match parsed {
            Query::Broken(why) => Some(view! { <p class="field-error search-error" role="alert">{why.clone()}</p> }),
            _ => None,
        })}
        <Show when=move || edited()>
            <div class="search-line">
                <span class="quiet">"The table shows what this search would hold."</span>
                <Show when=move || can_write && readable()>
                    <button type="button" class="button primary" on:click=move |_| save_changes.with_value(|save| save(()))>"Save changes"</button>
                </Show>
                <button type="button" class="button" on:click=revert>"Revert"</button>
            </div>
        </Show>
        <Show when=move || offer_save() && !saving.get()>
            <div class="search-line">
                <button type="button" class="button" on:click=move |_| saving.set(true)>"Save as smart catalogue…"</button>
            </div>
        </Show>
        <Show when=move || saving.get()>
            <SaveSearch shown query saving/>
        </Show>
        {error_line(error)}
    }
}

/// Names the new smart catalogue, and says where it searches.
#[component]
fn SaveSearch(
    shown: Memo<Route>,
    query: RwSignal<String>,
    saving: RwSignal<bool>,
) -> impl IntoView {
    let room = expect_context::<Room>();
    let name = RwSignal::new(String::new());
    let within = RwSignal::new(false);
    let error: Error = RwSignal::new(None);
    let here = match shown.get_untracked() {
        Route::Catalogue(uuid) => room
            .read_untracked(|txn| model::catalogue(txn, &uuid))
            .map(|catalogue| (uuid, catalogue.name)),
        _ => None,
    };
    let source = here.clone().map(|(uuid, _)| uuid);
    let create = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let text = query.get_untracked();
        let name = name.get_untracked();
        let source = within.get_untracked().then(|| source.clone()).flatten();
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
            {here.map(|(_, catalogue)| view! {
                <fieldset class="choice">
                    <label><input type="radio" name="within" prop:checked=move || !within.get()
                        on:change=move |_| within.set(false)/>" Search all the project's events"</label>
                    <label><input type="radio" name="within" prop:checked=move || within.get()
                        on:change=move |_| within.set(true)/>{format!(" Only events of “{catalogue}”")}</label>
                </fieldset>
            })}
            <p class="quiet">"It keeps every event that matches the search, as people edit the project."</p>
            {error_line(error)}
            <div class="row-buttons">
                <button type="submit" class="button primary">"Create"</button>
                <button type="button" class="button" on:click=move |_| saving.set(false)>"Cancel"</button>
            </div>
        </form>
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
