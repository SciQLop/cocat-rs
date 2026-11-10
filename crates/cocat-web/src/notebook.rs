//! A JupyterLab notebook document, read for display.

use std::collections::BTreeMap;

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellKind {
    Markdown,
    Code,
    Raw,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    Stream {
        error: bool,
        text: String,
    },
    Error {
        name: String,
        value: String,
        traceback: String,
    },
    Image {
        mime: String,
        base64: String,
    },
    Svg(String),
    Html(String),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub id: String,
    pub kind: CellKind,
    pub source: String,
    pub count: Option<i64>,
    pub outputs: Vec<Output>,
    /// Images pasted into the cell, by name, as `data:` URLs (raster images only).
    pub attachments: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notebook {
    pub language: String,
    pub cells: Vec<Cell>,
}

pub fn notebook(roots: &Value) -> Notebook {
    Notebook {
        language: language(&roots["meta"]),
        cells: roots["cells"]
            .as_array()
            .map_or_else(Vec::new, |cells| cells.iter().map(cell).collect()),
    }
}

fn language(meta: &Value) -> String {
    let metadata = &meta["metadata"];
    [
        &metadata["kernelspec"]["language"],
        &metadata["language_info"]["name"],
    ]
    .into_iter()
    .find_map(Value::as_str)
    .unwrap_or("python")
    .to_owned()
}

fn cell(cell: &Value) -> Cell {
    let kind = match cell["cell_type"].as_str() {
        Some("markdown") => CellKind::Markdown,
        Some("code") => CellKind::Code,
        _ => CellKind::Raw,
    };
    Cell {
        id: cell["id"].as_str().unwrap_or_default().to_owned(),
        kind,
        source: text(&cell["source"], ""),
        count: integer(&cell["execution_count"]),
        outputs: cell["outputs"].as_array().map_or_else(Vec::new, |outputs| {
            outputs.iter().filter_map(output).collect()
        }),
        attachments: attachments(&cell["attachments"]),
    }
}

const PASTED_IMAGES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

fn attachments(attachments: &Value) -> BTreeMap<String, String> {
    let Some(attachments) = attachments.as_object() else {
        return BTreeMap::new();
    };
    attachments
        .iter()
        .filter_map(|(name, bundle)| {
            let (mime, data) = PASTED_IMAGES
                .iter()
                .find_map(|mime| Some((*mime, bundle.get(*mime)?)))?;
            let base64: String = text(data, "")
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            Some((name.clone(), format!("data:{mime};base64,{base64}")))
        })
        .collect()
}

fn integer(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        value
            .as_f64()
            .filter(|f| f.fract() == 0.0)
            .map(|f| f as i64)
    })
}

/// nbformat allows a text field to be a string or a list of strings.
fn text(value: &Value, separator: &str) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(separator),
        _ => String::new(),
    }
}

fn plain(value: &Value, separator: &str) -> String {
    String::from_utf8_lossy(&strip_ansi_escapes::strip(text(value, separator))).into_owned()
}

fn output(output: &Value) -> Option<Output> {
    match output["output_type"].as_str()? {
        "stream" => Some(Output::Stream {
            error: output["name"] == "stderr",
            text: plain(&output["text"], ""),
        }),
        "error" => Some(Output::Error {
            name: text(&output["ename"], ""),
            value: text(&output["evalue"], ""),
            traceback: plain(&output["traceback"], "\n"),
        }),
        "display_data" | "execute_result" => rich(&output["data"]),
        _ => None,
    }
}

fn rich(data: &Value) -> Option<Output> {
    let get = |mime: &str| data.get(mime).map(|v| text(v, ""));
    ["image/png", "image/jpeg"]
        .into_iter()
        .find_map(|mime| {
            get(mime).map(|base64| Output::Image {
                mime: mime.to_owned(),
                base64: base64.split_whitespace().collect(),
            })
        })
        .or_else(|| get("image/svg+xml").map(Output::Svg))
        .or_else(|| get("text/html").map(Output::Html))
        .or_else(|| get("text/plain").map(Output::Text))
}

/// A sandboxed HTML output cannot measure itself, so its frame is sized from its table
/// rows: tall enough for a short table, and a long one scrolls inside its frame.
pub fn html_height(html: &str) -> u32 {
    let rows = u32::try_from(html.matches("<tr").count()).unwrap_or(u32::MAX);
    rows.saturating_mul(28).saturating_add(48).clamp(96, 640)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cell_with(outputs: Value) -> Notebook {
        notebook(&json!({"cells": [
            {"id": "a", "cell_type": "code", "source": "x", "execution_count": null, "outputs": outputs}
        ]}))
    }

    #[test]
    fn pasted_images_become_data_urls_and_svg_is_left_out() {
        let roots = json!({"cells": [{"id": "m", "cell_type": "markdown", "source": "x",
            "attachments": {"p.png": {"image/png": "QUJD\n"}, "s.svg": {"image/svg+xml": "<svg/>"}}}]});
        let cell = &notebook(&roots).cells[0];
        assert_eq!(
            cell.attachments.get("p.png").map(String::as_str),
            Some("data:image/png;base64,QUJD")
        );
        assert!(!cell.attachments.contains_key("s.svg"));
    }

    #[test]
    fn an_html_output_is_sized_by_its_rows() {
        assert_eq!(
            html_height("<p>hi</p>"),
            96,
            "a small output gets the minimum"
        );
        let table = format!("<table>{}</table>", "<tr><td>1</td></tr>".repeat(10));
        assert_eq!(html_height(&table), 10 * 28 + 48);
        let long = "<tr>".repeat(1000);
        assert_eq!(
            html_height(&long),
            640,
            "a long table scrolls inside its frame"
        );
    }

    #[test]
    fn cells_keep_their_order_and_kinds() {
        let nb = notebook(&json!({
            "meta": {"metadata": {"kernelspec": {"language": "julia"}}},
            "cells": [
                {"id": "a", "cell_type": "markdown", "source": "# Hi"},
                {"id": "b", "cell_type": "code", "source": "1+1", "execution_count": 3.0, "outputs": []},
                {"id": "c", "cell_type": "raw", "source": "r"},
                {"id": "d", "cell_type": "weird", "source": "w"}
            ]
        }));
        let kinds: Vec<_> = nb.cells.iter().map(|c| (c.id.as_str(), c.kind)).collect();
        assert_eq!(
            kinds,
            [
                ("a", CellKind::Markdown),
                ("b", CellKind::Code),
                ("c", CellKind::Raw),
                ("d", CellKind::Raw)
            ]
        );
        assert_eq!(nb.cells[1].count, Some(3));
        assert_eq!(nb.cells[0].count, None);
        assert_eq!(nb.cells[1].source, "1+1");
        assert_eq!(nb.language, "julia");
    }

    #[test]
    fn a_plot_prefers_png_over_text() {
        let nb = cell_with(json!([{"output_type": "display_data", "data": {
            "text/plain": "<Figure>", "image/png": "iVBO\nRw0K\n"}}]));
        assert_eq!(
            nb.cells[0].outputs,
            [Output::Image {
                mime: "image/png".into(),
                base64: "iVBORw0K".into()
            }]
        );
    }

    #[test]
    fn a_table_prefers_html_over_text() {
        let nb = cell_with(json!([{"output_type": "execute_result", "data": {
            "text/plain": "t", "text/html": ["<table>", "</table>"]}}]));
        assert_eq!(
            nb.cells[0].outputs,
            [Output::Html("<table></table>".into())]
        );
    }

    #[test]
    fn stream_text_may_be_a_list_of_lines() {
        let nb = cell_with(json!([
            {"output_type": "stream", "name": "stderr", "text": ["a\n", "b\n"]},
            {"output_type": "stream", "name": "stdout", "text": "c"},
            {"output_type": "unknown"}
        ]));
        assert_eq!(
            nb.cells[0].outputs,
            [
                Output::Stream {
                    error: true,
                    text: "a\nb\n".into()
                },
                Output::Stream {
                    error: false,
                    text: "c".into()
                }
            ]
        );
    }

    #[test]
    fn tracebacks_lose_their_colour_codes() {
        let nb = cell_with(json!([{"output_type": "error", "ename": "ValueError",
            "evalue": "bad", "traceback": ["\u{1b}[0;31mline1\u{1b}[0m", "line2"]}]));
        assert_eq!(
            nb.cells[0].outputs,
            [Output::Error {
                name: "ValueError".into(),
                value: "bad".into(),
                traceback: "line1\nline2".into()
            }]
        );
    }

    #[test]
    fn a_notebook_without_metadata_is_python() {
        let nb = notebook(&json!({}));
        assert_eq!(nb.language, "python");
        assert!(nb.cells.is_empty());
    }
}
