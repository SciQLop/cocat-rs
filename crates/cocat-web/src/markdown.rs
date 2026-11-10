//! Notebook markdown to HTML that is safe to insert into the page.

use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html};

const LINK_SCHEMES: [&str; 3] = ["http", "https", "mailto"];
const IMAGE_DATA_PREFIXES: [&str; 4] = [
    "data:image/png",
    "data:image/jpeg",
    "data:image/gif",
    "data:image/webp",
];

pub fn markdown_html(text: &str) -> String {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let mut refused = Vec::new();
    let events = Parser::new_ext(text, options).filter_map(|event| sanitize(event, &mut refused));
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

/// `refused` tracks, for each open link or image, whether its tags are being dropped.
fn sanitize<'a>(event: Event<'a>, refused: &mut Vec<bool>) -> Option<Event<'a>> {
    match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Some(Event::Text(raw)),
        Event::Start(tag @ (Tag::Link { .. } | Tag::Image { .. })) => {
            let drop = !destination_allowed(&tag);
            refused.push(drop);
            (!drop).then_some(Event::Start(tag))
        }
        Event::End(end @ (TagEnd::Link | TagEnd::Image)) => {
            (!refused.pop().unwrap_or(false)).then_some(Event::End(end))
        }
        other => Some(other),
    }
}

fn destination_allowed(tag: &Tag) -> bool {
    match tag {
        Tag::Link { dest_url, .. } => url_allowed(dest_url, &[]),
        Tag::Image { dest_url, .. } => url_allowed(dest_url, &IMAGE_DATA_PREFIXES),
        _ => false,
    }
}

/// Browsers ignore whitespace and control characters inside a scheme ("java\tscript:").
fn url_allowed(url: &CowStr, data_prefixes: &[&str]) -> bool {
    let url: String = url
        .chars()
        .filter(|c| !c.is_ascii_control() && !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    match url.split_once(':') {
        Some((scheme, _)) if !scheme.contains(['/', '?', '#']) => {
            LINK_SCHEMES.contains(&scheme) || data_prefixes.iter().any(|p| is_data_url(&url, p))
        }
        _ => true,
    }
}

fn is_data_url(url: &str, prefix: &str) -> bool {
    url.strip_prefix(prefix)
        .is_some_and(|rest| rest.starts_with([';', ',']))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_lists_and_tables_render() {
        let html =
            markdown_html("# Title\n\n- a\n- b\n\n| x | y |\n|---|---|\n| 1 | 2 |\n\n~~gone~~");
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("<li>a</li>"));
        assert!(html.contains("<table>"));
        assert!(html.contains("<del>gone</del>"));
    }

    #[test]
    fn raw_html_is_shown_as_text() {
        let block = markdown_html("<script>alert(1)</script>");
        assert!(!block.contains("<script"));
        assert!(block.contains("&lt;script&gt;"));
        let inline = markdown_html("hi <img src=x onerror=alert(1)> there");
        assert!(!inline.contains("<img"));
        assert!(inline.contains("&lt;img src=x onerror=alert(1)&gt;"));
    }

    #[test]
    fn a_javascript_link_loses_its_link() {
        let html = markdown_html(
            "[click](javascript:alert(1)) and [ok](https://a.b/c) [rel](x/y) [m](mailto:a@b.c)",
        );
        assert!(!html.contains("javascript"));
        assert!(html.contains("click"));
        assert!(html.contains(r#"<a href="https://a.b/c">ok</a>"#));
        assert!(html.contains(r#"<a href="x/y">rel</a>"#));
        assert!(html.contains(r#"<a href="mailto:a@b.c">m</a>"#));
    }

    #[test]
    fn a_data_image_stays_but_data_html_does_not() {
        let good = markdown_html("![pic](data:image/png;base64,AAAA)");
        assert!(good.contains(r#"<img src="data:image/png;base64,AAAA" alt="pic""#));
        let html = markdown_html("![evil](data:text/html;base64,AAAA)");
        assert!(!html.contains("<img"));
        assert!(!html.contains("data:"));
        assert!(html.contains("evil"));
        assert!(!markdown_html("![s](data:image/svg+xml;base64,AAAA)").contains("<img"));
        assert!(!markdown_html("[l](data:image/png;base64,AAAA)").contains("<a"));
    }

    #[test]
    fn a_tab_inside_the_scheme_does_not_slip_through() {
        let html = markdown_html(
            "[x](<java\tscript:alert(1)>) [y](<  JaVaScRiPt:alert(1)>) [z](<vbscript:x>)",
        );
        assert!(!html.contains("<a"));
    }

    #[test]
    fn an_entity_encoded_javascript_link_loses_its_link() {
        let html = markdown_html("[x](&#106;avascript:alert(1))");
        assert!(!html.contains("<a"));
        assert!(html.contains('x'));
    }

    #[test]
    fn an_autolink_to_javascript_is_plain_text() {
        let html = markdown_html("<javascript:alert(1)>");
        assert!(!html.contains("<a"));
        assert!(html.contains("javascript:alert(1)"));
    }

    #[test]
    fn a_reference_style_javascript_link_loses_its_link() {
        let html = markdown_html("[x][r]\n\n[r]: javascript:alert(1)");
        assert!(!html.contains("<a"));
    }
}
