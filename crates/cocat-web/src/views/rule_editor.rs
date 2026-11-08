//! A catalogue's rule as JSON, checked as it is typed and saved only when it reads. A
//! catalogue without one becomes smart once its first rule is saved.

use cocat_core::model;
use leptos::prelude::*;

use crate::edit::{self, rule_preview};
use crate::state::Room;
use crate::views::actions::Form;
use crate::views::controls::{Error, Hint, commit, error_line};

#[component]
pub fn RuleEditor(uuid: String, form: RwSignal<Form>) -> impl IntoView {
    let room = expect_context::<Room>();
    let current =
        room.read_untracked(|txn| model::catalogue(txn, &uuid).and_then(|found| found.rule));
    let adding = current.is_none();
    let text = RwSignal::new(edit::rule_text(current.as_ref()));
    let reader = room.clone();
    let preview = Memo::new(move |_| {
        let name = |uuid: &str| {
            reader
                .read(|txn| model::catalogue(txn, uuid))
                .map(|found| found.name)
        };
        text.with(|text| rule_preview(text, &name).map_err(|error| error.to_string()))
    });
    let error: Error = RwSignal::new(None);
    let save = move |_| {
        let rule = text.get_untracked();
        commit(&room, error, |txn| edit::set_rule(txn, &uuid, &rule));
        if error.with_untracked(Option::is_none) {
            form.set(Form::Closed);
        }
    };
    view! {
        <section class="inline-form rule-editor" aria-label="Rule editor">
            {adding.then(|| view! {
                <p class="quiet">"With a rule, this catalogue becomes smart: it keeps the events it lists and adds every event the rule picks, as people edit."</p>
            })}
            <label class="field">
                <span class="field-label">"Rule (JSON)"</span>
                <textarea class="mono" rows="8" spellcheck="false" prop:value=move || text.get()
                    on:input=move |event| text.set(event_target_value(&event))></textarea>
            </label>
            {move || match preview.get() {
                Ok(words) => view! { <p class="rule-ok">"Reads as: " {words}</p> }.into_any(),
                Err(problem) => view! { <p class="field-error" role="alert">{problem}</p> }.into_any(),
            }}
            <Hint anchor="smart-catalogues" topic="smart catalogues"
                text=r#"Example: {"filter": {"all": true}, "where": {"cmp": ["confidence", ">=", 0.8]}}. Tests: cmp, contains, match, has, and, or, not."#/>
            {error_line(error)}
            <div class="row-buttons">
                <button type="button" class="button primary" disabled=move || preview.with(Result::is_err)
                    on:click=save>"Save rule"</button>
                <button type="button" class="button" on:click=move |_| form.set(Form::Closed)>"Cancel"</button>
            </div>
        </section>
    }
}
