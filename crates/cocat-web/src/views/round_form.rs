//! Starting a round: one form, filled by a preset, with the catalogue of candidates and the
//! people's roles.

use cocat_core::room_id::is_public_id;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::catalogues::catalogue_list;
use crate::links::{Listed, sources};
use crate::public::PublicCatalogue;
use crate::rounds::{
    BudgetForm, DATA_UNITS, Need, NewRound, Next, PRESETS, RateRow, Round, Source, TIMES,
    creation_failure, next_step, voter_verb,
};
use crate::route::Route;
use crate::sharing::Member;
use crate::state::Room;
use crate::views::controls::{Error, error_line, go_to};
use crate::views::links::catalogues_of;
use crate::views::projects::Projects;
use crate::views::transfer::Importer;

/// One catalogue the visitor can read. A public one has no catalogue uuid yet: its project
/// holds a single catalogue, found when the round is made.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Choice {
    group: String,
    label: String,
    source: Source,
}

impl Choice {
    fn key(&self) -> String {
        format!("{}/{}", self.source.project, self.source.catalogue)
    }
}

fn listed_choices(group: &str, project: &str, listed: Vec<Listed>) -> Vec<Choice> {
    listed
        .into_iter()
        .filter(|catalogue| !catalogue.deleted)
        .map(|catalogue| Choice {
            group: group.to_owned(),
            label: catalogue.name,
            source: Source {
                project: project.to_owned(),
                catalogue: catalogue.uuid,
            },
        })
        .collect()
}

fn public_choices(index: Vec<PublicCatalogue>) -> Vec<Choice> {
    index
        .into_iter()
        .map(|entry| Choice {
            group: "Public catalogues".to_owned(),
            label: entry.name,
            source: Source {
                project: entry.id,
                catalogue: String::new(),
            },
        })
        .collect()
}

/// Other projects' catalogues and the public ones, read once; they arrive as they are read.
fn elsewhere(api: &str, others: Vec<crate::projects::Project>) -> RwSignal<Vec<Choice>> {
    let found = RwSignal::new(Vec::new());
    for project in others {
        let api = api.to_owned();
        spawn_local(async move {
            if let Ok(listed) = catalogues_of(&api, &project.id).await {
                let choices = listed_choices(&project.name, &project.id, listed);
                found.try_update(|found| found.extend(choices));
            }
        });
    }
    let api = api.to_owned();
    spawn_local(async move {
        if let Ok(index) = api::read::<Vec<PublicCatalogue>>(&api, &["public"]).await {
            found.try_update(|found| found.extend(public_choices(index)));
        }
    });
    found
}

// simplify: takes the first live catalogue of the public project, which holds one; ask the
// index for the catalogue uuid if public projects ever hold several.
/// The one catalogue a public project holds.
async fn public_catalogue(api: &str, source: Source) -> Result<Source, String> {
    if !is_public_id(&source.project) || !source.catalogue.is_empty() {
        return Ok(source);
    }
    let listed = catalogues_of(api, &source.project)
        .await
        .map_err(|e| e.to_string())?;
    let first = listed.into_iter().find(|catalogue| !catalogue.deleted);
    first
        .map(|catalogue| Source {
            catalogue: catalogue.uuid,
            ..source
        })
        .ok_or_else(|| "That public catalogue has nothing to copy.".to_owned())
}

async fn create(
    api: &str,
    room: &str,
    form: &NewRound,
    source: Option<Source>,
    just_imported: bool,
) -> Result<String, String> {
    let mut form = form.clone();
    form.candidates = match source {
        Some(source) => Some(public_catalogue(api, source).await?),
        None => None,
    };
    let body = form.request()?;
    let made: Round = api::create(api, &["rooms", room, "rounds"], body)
        .await
        .map_err(|error| creation_failure(&error.to_string(), just_imported))?;
    Ok(made.id)
}

// simplify: creating then setting the people are two calls, so a failure between them leaves
// a round with the default people; `created` makes a retry redo only the second. Add the
// members to the create request if that ever matters.
async fn start(
    api: &str,
    room: &str,
    form: &NewRound,
    source: Option<Source>,
    just_imported: bool,
    created: RwSignal<Option<String>>,
) -> Result<String, String> {
    let id = match next_step(created.get_untracked()) {
        Next::SetPeople(id) => id,
        Next::Create => {
            let id = create(api, room, form, source, just_imported).await?;
            created.try_set(Some(id.clone()));
            id
        }
    };
    let members = ["rooms", room, "rounds", &id, "members"];
    api::put(api, &members, form.members_request())
        .await
        .map_err(|error| {
            format!(
                "The round was made, but its people were not set: {error}. Start again to retry."
            )
        })?;
    Ok(id)
}

#[component]
pub fn NewRoundForm() -> impl IntoView {
    let room = expect_context::<Room>();
    let projects = expect_context::<Projects>();
    let me = room.config.user_email.clone();
    let members = RwSignal::new(Vec::<Member>::new());
    let form = RwSignal::new(NewRound::new(PRESETS[0], &[], &me));
    let error: Error = RwSignal::new(None);
    let busy = RwSignal::new(false);
    let created = RwSignal::new(None::<String>);
    let api = projects.api();
    let here = room.config.room_id.clone();
    let others = elsewhere(
        &api,
        projects.list.with_untracked(|list| sources(list, &here)),
    );
    let reader = room.clone();
    let project = here.clone();
    let choices = Memo::new(move |_| {
        let own = reader.read(|txn| catalogue_list(txn));
        let own = own.catalogues.into_iter().map(|item| Choice {
            group: "This project".to_owned(),
            label: item.name,
            source: Source {
                project: project.clone(),
                catalogue: item.uuid,
            },
        });
        own.chain(others.get()).collect::<Vec<_>>()
    });
    {
        let (api, here, me) = (api.clone(), here.clone(), me.clone());
        spawn_local(async move {
            if let Ok(listed) = api::read::<Vec<Member>>(&api, &["rooms", &here, "members"]).await {
                members.try_set(listed.clone());
                form.try_update(|form| *form = form.clone().with_preset(form.preset, &listed, &me));
            }
        });
    }
    let importer = expect_context::<Importer>();
    let imported_into = here.clone();
    importer.imported.set(None);
    Effect::new(move |_| {
        if let Some(catalogue) = importer.imported.get() {
            let source = Source {
                project: imported_into.clone(),
                catalogue,
            };
            form.update(|form| form.candidates = Some(source));
        }
    });
    let choose_preset = {
        let me = me.clone();
        Callback::new(move |index: usize| {
            let listed = members.get_untracked();
            form.update(|form| *form = form.clone().with_preset(PRESETS[index], &listed, &me));
        })
    };
    let submit = {
        let (api, here) = (api.clone(), here.clone());
        move |event: leptos::ev::SubmitEvent| {
            event.prevent_default();
            let (api, here) = (api.clone(), here.clone());
            let (current, source) =
                form.with_untracked(|form| (form.clone(), form.candidates.clone()));
            let just_imported = source.as_ref().is_some_and(|source| {
                importer.imported.get_untracked().as_deref() == Some(source.catalogue.as_str())
            });
            busy.set(true);
            error.set(None);
            spawn_local(async move {
                match start(&api, &here, &current, source, just_imported, created).await {
                    Ok(id) => go_to(&Route::Round(id).hash()),
                    Err(why) => {
                        error.try_set(Some(why));
                        busy.try_set(false);
                    }
                }
            });
        }
    };
    let page = window().location().pathname().unwrap_or_default();
    let back = Route::Rounds.href(&page);
    view! {
        <div class="groups-head">
            <h1>"New round"</h1>
            <a class="button" href=back.clone()>"Back to rounds"</a>
        </div>
        <form class="name-form round-form" on:submit=submit>
            <PresetPicker form choose=choose_preset/>
            <label>
                <span class="field-label">"Name"</span>
                <input type="text" prop:value=move || form.with(|f| f.name.clone())
                    on:input=move |event| form.update(|f| f.name = event_target_value(&event))/>
            </label>
            <label>
                <span class="field-label">"Deadline (optional, midnight UTC)"</span>
                <input type="date" prop:value=move || form.with(|f| f.deadline.clone())
                    on:input=move |event| form.update(|f| f.deadline = event_target_value(&event))/>
            </label>
            <Visibility form/>
            <BudgetSection form/>
            <Candidates form choices importer/>
            <People form/>
            {error_line(error)}
            <div class="row-buttons">
                <button type="submit" class="button primary" disabled=move || busy.get()>"Start round"</button>
                <a class="button" href=back>"Cancel"</a>
            </div>
        </form>
    }
}

#[component]
fn PresetPicker(form: RwSignal<NewRound>, choose: Callback<usize>) -> impl IntoView {
    let options = PRESETS.iter().enumerate().map(|(index, preset)| {
        view! {
            <label class="inline-choice">
                <input type="radio" name="preset"
                    prop:checked=move || form.with(|f| f.preset == *preset)
                    on:change=move |_| choose.run(index)/>
                {preset.label}
            </label>
        }
    });
    view! {
        <fieldset class="round-presets">
            <legend class="field-label">"What kind of round?"</legend>
            {options.collect_view()}
        </fieldset>
    }
}

#[component]
fn Visibility(form: RwSignal<NewRound>) -> impl IntoView {
    let option = move |value: &'static str, label: &'static str| {
        view! {
            <label class="inline-choice">
                <input type="radio" name="visibility"
                    prop:checked=move || form.with(|f| f.visibility == value)
                    on:change=move |_| form.update(|f| f.visibility = value)/>
                {label}
            </label>
        }
    };
    view! {
        <fieldset class="round-presets">
            <legend class="field-label">"Who sees each person's work before the round ends?"</legend>
            {option("open", "Everyone in the project (open)")}
            {option("blind", "Only its author, the deciders and the owners (blind)")}
        </fieldset>
    }
}

#[component]
fn BudgetSection(form: RwSignal<NewRound>) -> impl IntoView {
    let shown = move || form.with(|f| f.budget.is_some());
    let edit = move |change: Box<dyn Fn(&mut BudgetForm)>| {
        form.update(|f| f.budget.iter_mut().for_each(&change));
    };
    let rows = move || form.with(|f| f.budget.as_ref().map_or(0, |budget| budget.rates.len()));
    view! {
        <Show when=shown>
            <fieldset class="round-budget">
                <legend class="field-label">"Budget"</legend>
                <label>
                    <span class="field-label">"Event attribute that sets the cost (for example mode)"</span>
                    <input type="text"
                        prop:value=move || form.with(|f| f.budget.as_ref().map(|b| b.attribute.clone()).unwrap_or_default())
                        on:input=move |event| { let text = event_target_value(&event); edit(Box::new(move |b| b.attribute = text.clone())) }/>
                </label>
                <div class="amount">
                    <label>
                        <span class="field-label">"Cap"</span>
                        <input type="text" inputmode="decimal"
                            prop:value=move || form.with(|f| f.budget.as_ref().map(|b| b.cap.clone()).unwrap_or_default())
                            on:input=move |event| { let text = event_target_value(&event); edit(Box::new(move |b| b.cap = text.clone())) }/>
                    </label>
                    <UnitSelect units=&DATA_UNITS
                        value=Signal::derive(move || form.with(|f| f.budget.as_ref().map(|b| b.cap_unit.clone()).unwrap_or_default()))
                        on_pick=move |unit| edit(Box::new(move |b| b.cap_unit = unit.clone()))/>
                </div>
                <For each=move || 0..rows() key=|index| *index let:index>
                    <RateLine form index/>
                </For>
                <button type="button" class="button"
                    on:click=move |_| edit(Box::new(|b| b.rates.push(RateRow { unit: "Mibyte".into(), per: "min".into(), ..RateRow::default() })))>
                    "Add a rate"
                </button>
            </fieldset>
        </Show>
    }
}

#[component]
fn UnitSelect(
    units: &'static [&'static str],
    value: Signal<String>,
    on_pick: impl Fn(String) + 'static,
) -> impl IntoView {
    view! {
        <select on:change=move |event| on_pick(event_target_value(&event))>
            {units.iter().map(|unit| view! { <option value=*unit selected=move || value.get() == *unit>{*unit}</option> }).collect_view()}
        </select>
    }
}

#[component]
fn RateLine(form: RwSignal<NewRound>, index: usize) -> impl IntoView {
    let row = move || form.with(|f| f.budget.as_ref().and_then(|b| b.rates.get(index).cloned()));
    let set = move |change: Box<dyn Fn(&mut RateRow)>| {
        form.update(|f| {
            let rate = f.budget.as_mut().and_then(|b| b.rates.get_mut(index));
            rate.into_iter().for_each(&change);
        });
    };
    let text = move |event| event_target_value(&event);
    view! {
        <div class="amount rate-line">
            <input type="text" placeholder="value, e.g. burst" aria-label="Attribute value"
                prop:value=move || row().map(|r| r.value).unwrap_or_default()
                on:input=move |event| { let v = text(event); set(Box::new(move |r| r.value = v.clone())) }/>
            <input type="text" inputmode="decimal" placeholder="amount" aria-label="Amount"
                prop:value=move || row().map(|r| r.amount).unwrap_or_default()
                on:input=move |event| { let v = text(event); set(Box::new(move |r| r.amount = v.clone())) }/>
            <UnitSelect units=&DATA_UNITS value=Signal::derive(move || row().map(|r| r.unit).unwrap_or_default())
                on_pick=move |unit| set(Box::new(move |r| r.unit = unit.clone()))/>
            <span class="quiet">"per"</span>
            <UnitSelect units=&TIMES value=Signal::derive(move || row().map(|r| r.per).unwrap_or_default())
                on_pick=move |per| set(Box::new(move |r| r.per = per.clone()))/>
        </div>
    }
}

#[component]
fn Candidates(
    form: RwSignal<NewRound>,
    choices: Memo<Vec<Choice>>,
    importer: Importer,
) -> impl IntoView {
    let required = move || form.with(|f| f.preset.candidates == Need::Required);
    let selected = move || {
        form.with(|f| {
            f.candidates
                .as_ref()
                .map(|s| format!("{}/{}", s.project, s.catalogue))
        })
    };
    let pick = move |event| {
        let key = event_target_value(&event);
        let source = choices.with_untracked(|all| {
            all.iter()
                .find(|c| c.key() == key)
                .map(|c| c.source.clone())
        });
        form.update(|f| f.candidates = source);
    };
    let groups = move || {
        let all = choices.get();
        let mut names: Vec<String> = all.iter().map(|c| c.group.clone()).collect();
        names.dedup();
        names
            .into_iter()
            .map(|name| {
                let options = all
                    .iter()
                    .filter(|c| c.group == name)
                    .map(|c| {
                        let key = c.key();
                        let chosen = selected().as_deref() == Some(key.as_str());
                        view! { <option value=key selected=chosen>{c.label.clone()}</option> }
                    })
                    .collect_view();
                view! { <optgroup label=name>{options}</optgroup> }
            })
            .collect_view()
    };
    view! {
        <div class="round-candidates">
            <label>
                <span class="field-label">
                    {move || if required() { "Catalogue to review" } else { "Catalogue of candidates (optional)" }}
                </span>
                <select on:change=pick>
                    <option value="" selected=move || selected().is_none()>"None"</option>
                    {groups}
                </select>
            </label>
            <p class="hint">"The round copies this catalogue's events, so nobody edits the original by reviewing."</p>
            <button type="button" class="button" on:click=move |_| importer.open(None)>"Import a file…"</button>
        </div>
    }
}

#[component]
fn People(form: RwSignal<NewRound>) -> impl IntoView {
    let count = move || form.with(|f| f.seats.len());
    let voters = move || voter_verb(form.with(|f| f.kind()));
    view! {
        <fieldset class="round-people">
            <legend class="field-label">"People"</legend>
            <table>
                <thead><tr><th></th><th>"Proposes"</th><th>{voters}</th><th>"Decides"</th></tr></thead>
                <tbody>
                    <For each=move || 0..count() key=|index| *index let:index>
                        <SeatRow form index/>
                    </For>
                </tbody>
            </table>
        </fieldset>
    }
}

#[component]
fn SeatRow(form: RwSignal<NewRound>, index: usize) -> impl IntoView {
    let seat = move || form.with(|f| f.seats.get(index).cloned());
    let toggle = move |role: fn(&mut crate::rounds::Seat) -> &mut bool| {
        move |event: leptos::ev::Event| {
            let on = event_target_checked(&event);
            form.update(|f| {
                f.seats
                    .get_mut(index)
                    .into_iter()
                    .for_each(|seat| *role(seat) = on)
            });
        }
    };
    view! {
        <tr>
            <td>{move || seat().map(|s| s.email)}</td>
            <td><input type="checkbox" aria-label="Proposes" prop:checked=move || seat().is_some_and(|s| s.proposer)
                on:change=toggle(|s| &mut s.proposer)/></td>
            <td><input type="checkbox" aria-label="Votes" prop:checked=move || seat().is_some_and(|s| s.voter)
                on:change=toggle(|s| &mut s.voter)/></td>
            <td><input type="checkbox" aria-label="Decides" prop:checked=move || seat().is_some_and(|s| s.decider)
                disabled=move || seat().is_some_and(|s| !s.can_decide)
                on:change=toggle(|s| &mut s.decider)/></td>
        </tr>
    }
}
