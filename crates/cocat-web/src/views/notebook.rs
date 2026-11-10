//! A notebook as the workspace viewer shows it: rendered markdown, coloured code, outputs.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use leptos::prelude::*;

use crate::highlight::{highlight, worth_colouring};
use crate::markdown::markdown_html_with;
use crate::notebook::{Cell, CellKind, Notebook, Output, html_height};

/// Where an image a markdown cell names by a relative path lives, if the workspace has it.
pub type Place = std::sync::Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

#[component]
pub fn NotebookView(notebook: Notebook, place: Place) -> impl IntoView {
    let language = notebook.language.clone();
    notebook
        .cells
        .into_iter()
        .map(|cell| cell_view(cell, &language, &place))
        .collect_view()
}

fn cell_view(cell: Cell, language: &str, place: &Place) -> AnyView {
    match cell.kind {
        CellKind::Markdown => {
            let attachments = cell.attachments;
            let place = place.clone();
            let html = markdown_html_with(&cell.source, &move |dest: &str| match dest
                .strip_prefix("attachment:")
            {
                Some(name) => attachments.get(name).cloned(),
                None => place(dest),
            });
            view! { <div class="cell markdown-cell" inner_html=html></div> }.into_any()
        }
        CellKind::Code => {
            let count = cell
                .count
                .map_or_else(|| "[ ]".to_owned(), |n| format!("[{n}]"));
            view! {
                <div class="cell code-cell">
                    <div class="cell-input">
                        <span class="prompt" aria-hidden="true">{count}</span>
                        <pre class="code"><code>{coloured(&cell.source, language)}</code></pre>
                    </div>
                    {(!cell.outputs.is_empty()).then(|| view! {
                        <div class="cell-outputs">{cell.outputs.into_iter().map(output_view).collect_view()}</div>
                    })}
                </div>
            }
            .into_any()
        }
        CellKind::Raw => view! { <pre class="cell raw-cell">{cell.source}</pre> }.into_any(),
    }
}

/// Code as tone runs, one line per line; plain when colouring would stall the page.
pub fn coloured(code: &str, language: &str) -> AnyView {
    if !worth_colouring(code) {
        return code.to_owned().into_any();
    }
    let lines = highlight(code, language);
    let last = lines.len().saturating_sub(1);
    lines
        .into_iter()
        .enumerate()
        .map(|(index, runs)| {
            view! {
                <span class="line">
                    {runs.into_iter().map(|(tone, text)| view! { <span class=tone.class()>{text}</span> }).collect_view()}
                </span>
                {(index < last).then_some("\n")}
            }
        })
        .collect_view()
        .into_any()
}

/// Nothing an HTML output names is fetched: no remote stylesheet, no tracking pixel.
const OUTPUT_POLICY: &str = "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; img-src data:\">";

/// The look of an HTML output inside its sandbox: the app's font, table rules and theme.
const OUTPUT_STYLE: &str = "<style>\
:root { color-scheme: light dark; }\
body { margin: 8px; font: 13px/1.5 Inter, system-ui, sans-serif; color: #1c2331; background: #ffffff; }\
table { border-collapse: collapse; }\
th, td { border: 1px solid #d5d9e0; padding: 4px 10px; text-align: right; }\
th { background: #f5f6f8; }\
@media (prefers-color-scheme: dark) {\
  body { color: #e6e9ef; background: #181e29; }\
  th, td { border-color: #2c3445; } th { background: #1d2431; }\
}\
</style>";

fn output_view(output: Output) -> AnyView {
    match output {
        Output::Stream { error, text } => {
            view! { <pre class="output stream" class:stderr=error>{text}</pre> }.into_any()
        }
        Output::Error {
            name,
            value,
            traceback,
        } => view! {
            <pre class="output traceback"><strong>{format!("{name}: {value}")}</strong>"\n"{traceback}</pre>
        }
        .into_any(),
        Output::Image { mime, base64 } => {
            view! { <img class="output image-output" alt="" src=format!("data:{mime};base64,{base64}")/> }
                .into_any()
        }
        Output::Svg(svg) => {
            let src = format!("data:image/svg+xml;base64,{}", STANDARD.encode(svg));
            view! { <img class="output image-output" alt="" src=src/> }.into_any()
        }
        Output::Html(html) => {
            let height = format!("height: {}px", html_height(&html));
            let html = format!("{OUTPUT_POLICY}{OUTPUT_STYLE}{html}");
            // An empty sandbox: the output's own scripts and links never reach the page.
            view! { <iframe class="output html-output" sandbox="" srcdoc=html style=height title="Output"></iframe> }
                .into_any()
        }
        Output::Text(text) => view! { <pre class="output">{text}</pre> }.into_any(),
    }
}
