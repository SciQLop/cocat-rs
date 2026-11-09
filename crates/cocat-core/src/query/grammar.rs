//! The query syntax, read into a tree that keeps where each piece is, so the meaning pass
//! can point at it. Precedence, loosest first: OR, AND (or a space), NOT.

use std::ops::Range;

use winnow::ascii::multispace0;
use winnow::combinator::{alt, cut_err, not, opt, preceded, repeat, terminated};
use winnow::error::{ContextError, ErrMode, StrContext, StrContextValue};
use winnow::prelude::*;
use winnow::stream::LocatingSlice;
use winnow::token::{none_of, one_of, take_while};

use super::QueryError;
use crate::rules::Op;

type Input<'t> = LocatingSlice<&'t str>;

/// The words the grammar keeps for itself; `TO` only inside a range.
pub const KEYWORDS: [&str; 3] = ["AND", "OR", "NOT"];
/// What a bare word cannot hold; a bare value cannot hold brackets either.
pub const WORD_STOPS: [char; 3] = ['(', ')', '"'];
pub const VALUE_STOPS: [char; 5] = ['(', ')', '"', '[', ']'];

#[derive(Debug)]
pub enum Ast {
    Or(Vec<Ast>),
    And(Vec<Ast>),
    Not(Box<Ast>),
    Word(Scalar),
    Exists(Name),
    Field(Name, Term),
}

#[derive(Debug)]
pub struct Name {
    pub text: String,
    pub at: Range<usize>,
}

/// What follows `field:`.
#[derive(Debug)]
pub enum Term {
    Regex(Scalar),
    /// `None`: no operator, the field's own way to match.
    Single(Option<Op>, Scalar),
    Range(Scalar, Scalar),
}

#[derive(Debug)]
pub struct Scalar {
    pub text: String,
    pub quoted: bool,
    pub at: Range<usize>,
}

impl Scalar {
    /// `*`: an open end of a range.
    pub fn is_open(&self) -> bool {
        !self.quoted && self.text == "*"
    }
}

/// The tree of `text`, `None` for an empty query.
pub fn tree(text: &str) -> Result<Option<Ast>, QueryError> {
    terminated(preceded(multispace0, opt(or)), multispace0)
        .parse(LocatingSlice::new(text))
        .map_err(|error| {
            let at = error.offset();
            QueryError::at(text, at, message(error.inner(), &text[at..]))
        })
}

fn message(error: &ContextError, rest: &str) -> String {
    let expected = error.context().find_map(|context| match context {
        StrContext::Expected(StrContextValue::Description(what)) => Some(*what),
        _ => None,
    });
    match (expected, rest.chars().next()) {
        (Some(what), _) => format!("expected {what}"),
        (None, Some(found)) => format!("unexpected “{found}”"),
        (None, None) => "the query ends too soon".to_owned(),
    }
}

fn expected(what: &'static str) -> StrContext {
    StrContext::Expected(StrContextValue::Description(what))
}

fn or(input: &mut Input) -> ModalResult<Ast> {
    let first = and.parse_next(input)?;
    let rest: Vec<Ast> = repeat(
        0..,
        preceded(
            keyword("OR"),
            cut_err(and).context(expected("a search term after OR")),
        ),
    )
    .parse_next(input)?;
    Ok(joined(first, rest, Ast::Or))
}

fn and(input: &mut Input) -> ModalResult<Ast> {
    let first = unary.parse_next(input)?;
    let after_and = preceded(
        keyword("AND"),
        cut_err(unary).context(expected("a search term after AND")),
    );
    let rest: Vec<Ast> = repeat(0.., alt((after_and, unary))).parse_next(input)?;
    Ok(joined(first, rest, Ast::And))
}

fn joined(first: Ast, mut rest: Vec<Ast>, join: fn(Vec<Ast>) -> Ast) -> Ast {
    if rest.is_empty() {
        return first;
    }
    rest.insert(0, first);
    join(rest)
}

fn unary(input: &mut Input) -> ModalResult<Ast> {
    let negated = preceded(
        keyword("NOT"),
        cut_err(unary).context(expected("a search term after NOT")),
    );
    alt((negated.map(|inner| Ast::Not(Box::new(inner))), primary)).parse_next(input)
}

fn primary(input: &mut Input) -> ModalResult<Ast> {
    preceded(
        multispace0,
        alt((
            group,
            exists,
            field,
            phrase.map(Ast::Word),
            word.map(Ast::Word),
        )),
    )
    .parse_next(input)
}

/// A keyword on its own: not the start of a longer word.
fn keyword<'t>(name: &'static str) -> impl Parser<Input<'t>, (), ErrMode<ContextError>> {
    (multispace0, name, not(one_of(|c: char| is_word_char(c)))).void()
}

fn group(input: &mut Input) -> ModalResult<Ast> {
    preceded(
        '(',
        cut_err(terminated(
            preceded(multispace0, or).context(expected("a search term")),
            (multispace0, ')').context(expected("a closing )")),
        )),
    )
    .parse_next(input)
}

fn exists(input: &mut Input) -> ModalResult<Ast> {
    preceded("_exists_:", cut_err(name).context(expected("a field name")))
        .map(Ast::Exists)
        .parse_next(input)
}

fn field(input: &mut Input) -> ModalResult<Ast> {
    let (name, _) = (name, ':').parse_next(input)?;
    let term = cut_err(term)
        .context(expected("a value"))
        .parse_next(input)?;
    Ok(Ast::Field(name, term))
}

/// A field (`author`) or an attribute (`#density`).
fn name(input: &mut Input) -> ModalResult<Name> {
    let identifier = (
        one_of(|c: char| c.is_ascii_alphabetic() || c == '_'),
        take_while(0.., |c: char| c.is_ascii_alphanumeric() || c == '_'),
    );
    let attribute = ('#', take_while(1.., |c: char| is_word_char(c) && c != ':'));
    alt((attribute.take(), identifier.take()))
        .with_span()
        .map(|(text, at): (&str, _)| Name {
            text: text.to_owned(),
            at,
        })
        .parse_next(input)
}

fn term(input: &mut Input) -> ModalResult<Term> {
    alt((regex.map(Term::Regex), range, single)).parse_next(input)
}

fn single(input: &mut Input) -> ModalResult<Term> {
    let op = alt((
        ">=".value(Op::Ge),
        "<=".value(Op::Le),
        ">".value(Op::Gt),
        "<".value(Op::Lt),
        "=".value(Op::Eq),
    ));
    let (op, value) = (opt(op), scalar).parse_next(input)?;
    Ok(Term::Single(op, value))
}

fn range(input: &mut Input) -> ModalResult<Term> {
    let bounds = (
        preceded(multispace0, scalar),
        keyword("TO").context(expected("TO")),
        preceded(multispace0, scalar).context(expected("an upper bound")),
        (multispace0, ']').context(expected("a closing ]")),
    );
    preceded('[', cut_err(bounds))
        .map(|(low, _, high, _)| Term::Range(low, high))
        .parse_next(input)
}

fn scalar(input: &mut Input) -> ModalResult<Scalar> {
    let bare = take_while(1.., |c: char| {
        !c.is_whitespace() && !VALUE_STOPS.contains(&c)
    })
    .map(|text: &str| (text.to_owned(), false));
    let quoted = phrase.map(|scalar| (scalar.text, true));
    alt((quoted, bare))
        .with_span()
        .map(|((text, quoted), at)| Scalar { text, quoted, at })
        .parse_next(input)
}

/// `"..."`, where `\"` is a quote and `\\` a backslash.
fn phrase(input: &mut Input) -> ModalResult<Scalar> {
    let character = alt(("\\\"".value('"'), "\\\\".value('\\'), none_of('"')));
    let body = repeat(0.., character).fold(String::new, |mut text, c| {
        text.push(c);
        text
    });
    preceded(
        '"',
        cut_err(terminated(body, '"')).context(expected("a closing \"")),
    )
    .with_span()
    .map(|(text, at)| Scalar {
        text,
        quoted: true,
        at,
    })
    .parse_next(input)
}

/// `/.../`, where `\/` is a slash.
fn regex(input: &mut Input) -> ModalResult<Scalar> {
    let piece = alt(("\\/".value("/"), "\\\\".value("\\\\"), none_of('/').take()));
    let body = repeat(0.., piece).fold(String::new, |mut text, piece: &str| {
        text.push_str(piece);
        text
    });
    preceded(
        '/',
        cut_err(terminated(body, '/')).context(expected("a closing /")),
    )
    .with_span()
    .map(|(text, at)| Scalar {
        text,
        quoted: true,
        at,
    })
    .parse_next(input)
}

fn word(input: &mut Input) -> ModalResult<Scalar> {
    let start = input.checkpoint();
    let text = take_while(1.., is_word_char)
        .with_span()
        .parse_next(input)?;
    let (text, at): (&str, _) = text;
    if KEYWORDS.contains(&text) {
        input.reset(&start);
        return Err(ErrMode::Backtrack(ContextError::new()));
    }
    Ok(Scalar {
        text: text.to_owned(),
        quoted: false,
        at,
    })
}

pub fn is_word_char(c: char) -> bool {
    !c.is_whitespace() && !WORD_STOPS.contains(&c)
}
