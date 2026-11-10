//! Syntax colouring: code in, lines of (tone, text) runs out.

use std::sync::OnceLock;
use syntect::easy::ScopeRangeIterator;
use syntect::parsing::{ParseState, ScopeStack, SyntaxSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Keyword,
    String,
    Number,
    Comment,
    Function,
    Type,
    Punctuation,
}

/// First matching prefix wins; `punctuation.definition` is left out so the quotes of a
/// string or the `#` of a comment take the tone of what they delimit.
const TONES: &[(&str, Tone)] = &[
    ("comment", Tone::Comment),
    ("string", Tone::String),
    ("constant.numeric", Tone::Number),
    ("entity.name.function", Tone::Function),
    ("support.function", Tone::Function),
    ("storage.type.function", Tone::Keyword),
    ("storage.type.class", Tone::Keyword),
    ("entity.name.type", Tone::Type),
    ("entity.name.class", Tone::Type),
    ("support.type", Tone::Type),
    ("support.class", Tone::Type),
    ("storage.type", Tone::Type),
    ("keyword", Tone::Keyword),
    ("storage", Tone::Keyword),
];

impl Tone {
    /// The stylesheet class that colours this tone.
    pub fn class(self) -> &'static str {
        match self {
            Tone::Plain => "tone-plain",
            Tone::Keyword => "tone-keyword",
            Tone::String => "tone-string",
            Tone::Number => "tone-number",
            Tone::Comment => "tone-comment",
            Tone::Function => "tone-function",
            Tone::Type => "tone-type",
            Tone::Punctuation => "tone-punctuation",
        }
    }
}

/// Colouring runs on the page's only thread: a huge or minified file shows plain instead.
pub fn worth_colouring(code: &str) -> bool {
    code.len() <= 200_000 && code.lines().all(|line| line.len() <= 5_000)
}

type Run = (Tone, String);

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn tone_of_scope(scope: &str) -> Option<Tone> {
    if scope.starts_with("punctuation.definition") {
        return None;
    }
    if scope.starts_with("punctuation") {
        return Some(Tone::Punctuation);
    }
    TONES
        .iter()
        .find(|(prefix, _)| scope.starts_with(prefix))
        .map(|(_, tone)| *tone)
}

fn tone_of(stack: &ScopeStack) -> Tone {
    stack
        .as_slice()
        .iter()
        .rev()
        .find_map(|scope| tone_of_scope(&scope.to_string()))
        .unwrap_or(Tone::Plain)
}

fn push_run(runs: &mut Vec<Run>, tone: Tone, text: &str) {
    if text.is_empty() {
        return;
    }
    match runs.last_mut() {
        Some((last, run)) if *last == tone => run.push_str(text),
        _ => runs.push((tone, text.to_string())),
    }
}

fn plain_lines(code: &str) -> Vec<Vec<Run>> {
    code.lines()
        .map(|line| vec![(Tone::Plain, line.to_string())])
        .collect()
}

fn highlight_line(
    line: &str,
    state: &mut ParseState,
    stack: &mut ScopeStack,
    set: &SyntaxSet,
) -> Vec<Run> {
    let Ok(ops) = state.parse_line(line, set) else {
        return vec![(Tone::Plain, line.trim_end_matches('\n').to_string())];
    };
    let mut runs = Vec::new();
    for (range, op) in ScopeRangeIterator::new(&ops, line) {
        if stack.apply(op).is_err() {
            break;
        }
        push_run(&mut runs, tone_of(stack), range_text(line, range));
    }
    runs
}

fn range_text(line: &str, range: std::ops::Range<usize>) -> &str {
    line[range].trim_end_matches('\n')
}

/// `language` is a syntect lookup token: a file extension or a language name.
pub fn highlight(code: &str, language: &str) -> Vec<Vec<Run>> {
    let set = syntaxes();
    let Some(syntax) = set.find_syntax_by_token(language) else {
        return plain_lines(code);
    };
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    code.lines()
        .map(|line| highlight_line(&format!("{line}\n"), &mut state, &mut stack, set))
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_tone_has_its_class() {
        assert_eq!(Tone::Keyword.class(), "tone-keyword");
        assert_eq!(Tone::Plain.class(), "tone-plain");
    }

    #[test]
    fn a_huge_or_minified_file_is_not_coloured() {
        assert!(worth_colouring("x = 1\n"));
        assert!(!worth_colouring(&"x".repeat(300_000)));
        assert!(!worth_colouring(&format!("a\n{}\n", "y".repeat(6_000))));
    }

    use super::*;

    fn tone_of(lines: &[Vec<(Tone, String)>], text: &str) -> Option<Tone> {
        lines
            .iter()
            .flatten()
            .find(|(_, run)| run.trim() == text)
            .map(|(tone, _)| *tone)
    }

    #[test]
    fn python_keywords_and_strings_get_their_tones() {
        let lines = highlight("def f(x):\n    return \"hi\" + 42  # note\n", "py");
        assert_eq!(tone_of(&lines, "def"), Some(Tone::Keyword));
        assert_eq!(tone_of(&lines, "return"), Some(Tone::Keyword));
        assert_eq!(tone_of(&lines, "\"hi\""), Some(Tone::String));
        assert_eq!(tone_of(&lines, "42"), Some(Tone::Number));
        assert_eq!(tone_of(&lines, "# note"), Some(Tone::Comment));
        assert_eq!(tone_of(&lines, "f"), Some(Tone::Function));
    }

    #[test]
    fn notebook_language_names_work() {
        let lines = highlight("import os", "python");
        assert_eq!(tone_of(&lines, "import"), Some(Tone::Keyword));
    }

    #[test]
    fn an_unknown_language_is_plain() {
        let lines = highlight("def f(x): return 1", "klingon");
        assert!(lines.iter().flatten().all(|(tone, _)| *tone == Tone::Plain));
        assert_eq!(lines[0].concat_text(), "def f(x): return 1");
    }

    #[test]
    fn lines_are_kept() {
        let lines = highlight("a = 1\n\nb = 'x'\n", "py");
        assert_eq!(lines.len(), 3);
        assert!(lines[1].is_empty() || lines[1].iter().all(|(_, t)| t.is_empty()));
        let text: Vec<String> = lines.iter().map(|l| l.concat_text()).collect();
        assert_eq!(text, ["a = 1", "", "b = 'x'"]);
    }

    #[test]
    fn only_toml_ini_and_ts_are_missing_from_the_default_syntaxes() {
        let tokens = [
            "py", "md", "toml", "json", "yaml", "ini", "sh", "rs", "c", "cpp", "js", "ts", "html",
            "css", "tex",
        ];
        let unresolved: Vec<_> = tokens
            .iter()
            .filter(|token| syntaxes().find_syntax_by_token(token).is_none())
            .collect();
        assert_eq!(unresolved, [&"toml", &"ini", &"ts"]);
    }

    trait ConcatText {
        fn concat_text(&self) -> String;
    }
    impl ConcatText for Vec<(Tone, String)> {
        fn concat_text(&self) -> String {
            self.iter().map(|(_, t)| t.as_str()).collect()
        }
    }
}
