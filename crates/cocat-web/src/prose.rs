//! A description as people and sources write it, made readable: paragraphs at blank lines,
//! `code` in backticks, links and DOIs made clickable, and the citation set apart.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Span {
    Text(String),
    Code(String),
    Link { href: String, text: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paragraph {
    /// Whom to cite: the published lists end their description with `Cite: …`.
    pub cite: bool,
    pub spans: Vec<Span>,
}

const CITE: &str = "Cite: ";

pub fn paragraphs(text: &str) -> Vec<Paragraph> {
    let (body, cite) = match text.split_once(&format!(" {CITE}")) {
        Some((body, cite)) => (body, Some(cite)),
        None => match text.strip_prefix(CITE) {
            Some(cite) => ("", Some(cite)),
            None => (text, None),
        },
    };
    let body = blocks(body).into_iter().map(|block| Paragraph {
        cite: false,
        spans: spans(&block),
    });
    let cite = cite.map(|cite| Paragraph {
        cite: true,
        spans: spans(cite.trim()),
    });
    body.chain(cite).collect()
}

/// The text's paragraphs: runs of lines between blank ones.
fn blocks(text: &str) -> Vec<String> {
    text.split('\n')
        .collect::<Vec<_>>()
        .split(|line| line.trim().is_empty())
        .filter(|lines| !lines.is_empty())
        .map(|lines| lines.join("\n").trim().to_owned())
        .collect()
}

/// Code between pairs of backticks; a lone backtick stays as it is.
fn spans(text: &str) -> Vec<Span> {
    let pieces: Vec<&str> = text.split('`').collect();
    let paired = pieces.len() % 2 == 1;
    let mut out = Vec::new();
    for (index, piece) in pieces.iter().enumerate() {
        let last = index == pieces.len() - 1;
        if index % 2 == 1 && (paired || !last) {
            out.push(Span::Code((*piece).to_owned()));
        } else {
            let piece = if index % 2 == 1 {
                format!("`{piece}")
            } else {
                (*piece).to_owned()
            };
            out.extend(linked(&piece));
        }
    }
    merged(out)
}

/// Neighbouring texts as one, and no empty text.
fn merged(spans: Vec<Span>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for span in spans {
        match (out.last_mut(), span) {
            (_, Span::Text(text)) if text.is_empty() => {}
            (Some(Span::Text(before)), Span::Text(text)) => before.push_str(&text),
            (_, span) => out.push(span),
        }
    }
    out
}

const LINK_STARTS: [&str; 3] = ["https://", "http://", "doi:10."];

/// Text with its web addresses and `doi:` references as links.
fn linked(text: &str) -> Vec<Span> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = LINK_STARTS
        .iter()
        .filter_map(|start| rest.find(start))
        .min()
    {
        let end = rest[at..]
            .find(char::is_whitespace)
            .map_or(rest.len(), |length| at + length);
        let shown = trimmed(&rest[at..end]);
        let href = match shown.strip_prefix("doi:") {
            Some(doi) => format!("https://doi.org/{doi}"),
            None => shown.to_owned(),
        };
        out.push(Span::Text(rest[..at].to_owned()));
        out.push(Span::Link {
            href,
            text: shown.to_owned(),
        });
        rest = &rest[at + shown.len()..];
    }
    out.push(Span::Text(rest.to_owned()));
    out
}

/// A link without the punctuation of the sentence around it: a final `.` or `,`, and a `)`
/// that closes no `(` of the link.
fn trimmed(link: &str) -> &str {
    let mut link = link.trim_end_matches(['.', ',', ';', ':', '!', '?', '"', '\'']);
    while link.ends_with(')') && link.matches('(').count() < link.matches(')').count() {
        link = link[..link.len() - 1].trim_end_matches(['.', ',', ';', ':']);
    }
    link
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str) -> Span {
        Span::Text(text.to_owned())
    }

    fn link(href: &str, shown: &str) -> Span {
        Span::Link {
            href: href.to_owned(),
            text: shown.to_owned(),
        }
    }

    #[test]
    fn backticks_mark_code_and_the_citation_is_its_own_paragraph() {
        let described = "`f_min_khz` and `f_max_khz` bound each burst. Cite: Fogg et al. (2023), PADC, https://doi.org/10.25935/ayzp-1833, licence CC-BY-4.0.";
        let paragraphs = paragraphs(described);
        assert_eq!(paragraphs.len(), 2);
        assert!(!paragraphs[0].cite);
        assert_eq!(
            paragraphs[0].spans,
            vec![
                Span::Code("f_min_khz".to_owned()),
                text(" and "),
                Span::Code("f_max_khz".to_owned()),
                text(" bound each burst."),
            ]
        );
        assert!(paragraphs[1].cite);
        assert_eq!(
            paragraphs[1].spans,
            vec![
                text("Fogg et al. (2023), PADC, "),
                link(
                    "https://doi.org/10.25935/ayzp-1833",
                    "https://doi.org/10.25935/ayzp-1833"
                ),
                text(", licence CC-BY-4.0."),
            ]
        );
    }

    #[test]
    fn a_doi_reference_links_to_doi_org_and_a_closing_parenthesis_stays_outside() {
        let spans = spans("dataset (doi:10.6084/m9.figshare.27952410).");
        assert_eq!(
            spans,
            vec![
                text("dataset ("),
                link(
                    "https://doi.org/10.6084/m9.figshare.27952410",
                    "doi:10.6084/m9.figshare.27952410"
                ),
                text(")."),
            ]
        );
    }

    #[test]
    fn a_parenthesis_inside_a_link_is_kept() {
        let spans = spans("see https://en.wikipedia.org/wiki/Flux_(physics).");
        assert_eq!(
            spans[1],
            link(
                "https://en.wikipedia.org/wiki/Flux_(physics)",
                "https://en.wikipedia.org/wiki/Flux_(physics)"
            )
        );
    }

    #[test]
    fn blank_lines_part_paragraphs_and_single_line_breaks_stay() {
        let paragraphs = paragraphs("First line\nsame paragraph\n \n Second one");
        assert_eq!(paragraphs.len(), 2);
        assert_eq!(
            paragraphs[0].spans,
            vec![text("First line\nsame paragraph")]
        );
        assert_eq!(paragraphs[1].spans, vec![text("Second one")]);
    }

    #[test]
    fn a_lone_backtick_is_plain_text() {
        assert_eq!(spans("it`s fine"), vec![text("it`s fine")]);
    }

    #[test]
    fn plain_text_is_one_paragraph() {
        let paragraphs = paragraphs("Just words.");
        assert_eq!(
            paragraphs,
            vec![Paragraph {
                cite: false,
                spans: vec![text("Just words.")]
            }]
        );
    }
}
