//! Ranks catalogues by how well their text answers a query, the way SciQLop ranks its products:
//! BM25F over two fields, a short title (name, folder, source) that counts six times as much as
//! the long prose (description, citation). A mission or topic named in the title then beats a
//! passing mention in someone's description.
//!
//! BM25F with an integer field weight is plain BM25 over a document holding that field's words
//! that many times: the term counts and the document length both scale. So the `bm25` crate
//! scores a document built that way, with this module's tokenizer instead of its default one,
//! whose stop-word lists and stemmers would weigh on the web app.

use bm25::{SearchEngine, SearchEngineBuilder, Tokenizer};

/// How much more a title word counts than a word of the prose, as in SciQLop's product search.
const TITLE_WEIGHT: usize = 6;

/// The two fields of a searched document.
#[derive(Clone, Copy, Debug)]
pub struct Fields<'a> {
    /// The few words that name it: name, folder, source.
    pub title: &'a str,
    /// What describes it: description, citation.
    pub body: &'a str,
}

/// Lowercase runs of letters and digits, plurals folded: `10.3847/2041-8213/ab4429` gives
/// `10`, `3847`, `2041`, `8213`, `ab4429`, so a DOI pasted whole finds its paper.
#[derive(Clone, Copy, Debug, Default)]
struct Words;

impl Tokenizer for Words {
    fn tokenize(&self, text: &str) -> Vec<String> {
        let words: Vec<String> = text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(|word| singular(&word.to_lowercase()).to_owned())
            .collect();
        let acronyms = acronyms(&words);
        words.into_iter().chain(acronyms).collect()
    }
}

/// Names the field writes both in full and as an acronym, in words as the tokenizer leaves them
/// (lowercase, singular), and other spellings of one acronym. Text holding a name also gets
/// its acronym, so either finds both, while the words written out still count on their own.
const ACRONYMS: &[(&[&str], &str)] = &[
    (&["interplanetary", "coronal", "mass", "ejection"], "icme"),
    (&["coronal", "mass", "ejection"], "cme"),
    (&["icmecat"], "icme"),
    (&["solar", "orbiter"], "solo"),
    (&["parker", "solar", "probe"], "psp"),
    (&["heliospheric", "current", "sheet"], "hcs"),
    (&["solar", "energetic", "particle"], "sep"),
    (&["stream", "interaction", "region"], "sir"),
    (&["corotating", "interaction", "region"], "sir"),
    (&["cir"], "sir"),
    (&["sircat"], "sir"),
];

/// The acronyms of the names `words` hold. A name inside a longer one counts once, as the
/// longer one: an interplanetary CME is an ICME, not also a CME.
fn acronyms(words: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    let mut covered = 0;
    for at in 0..words.len() {
        if at < covered {
            continue;
        }
        let names = |phrase: &[&str]| {
            words.len() >= at + phrase.len()
                && phrase
                    .iter()
                    .zip(&words[at..])
                    .all(|(name, word)| name == word)
        };
        if let Some((phrase, acronym)) = ACRONYMS.iter().find(|(phrase, _)| names(phrase)) {
            found.push((*acronym).to_owned());
            covered = at + phrase.len();
        }
    }
    found
}

// simplify: a final `s` dropped is all the stemming there is (`icmes` → `icme`, `mass` kept);
// documents and queries fold alike, so it only has to be consistent. A Snowball stemmer
// (`rust-stemmers`) when `-ing` or `-ion` forms start to matter.
fn singular(word: &str) -> &str {
    match word.strip_suffix('s') {
        Some(stem) if stem.len() >= 3 && !stem.ends_with('s') => stem,
        _ => word,
    }
}

/// The documents, indexed once; ask it for each query.
pub struct TextIndex {
    engine: SearchEngine<u32, u32, Words>,
}

impl TextIndex {
    pub fn new<'a>(documents: impl IntoIterator<Item = Fields<'a>>) -> Self {
        let corpus = documents.into_iter().map(|fields| {
            let mut text = format!("{} ", fields.title).repeat(TITLE_WEIGHT);
            text.push_str(fields.body);
            text
        });
        Self {
            engine: SearchEngineBuilder::<u32, u32, Words>::with_tokenizer_and_corpus(
                Words, corpus,
            )
            .build(),
        }
    }

    /// The documents that hold at least one word of `query`, by their position in the order
    /// given to [`TextIndex::new`], best first, with their score.
    pub fn search(&self, query: &str) -> Vec<(usize, f32)> {
        self.engine
            .search(query, None)
            .into_iter()
            .map(|found| (found.document.id as usize, found.score))
            .collect()
    }
}

/// A catalogue as the search reads it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Catalogue<'a> {
    pub name: &'a str,
    pub folder: Option<&'a str>,
    pub source: Option<&'a str>,
    pub description: Option<&'a str>,
}

impl Catalogue<'_> {
    /// What names it: name, folder and source.
    fn title(&self) -> String {
        [Some(self.name), self.folder, self.source]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Holds every one of `words` (lowercase), whole or as the start of a longer word.
    fn holds(&self, words: &[String]) -> bool {
        let text =
            format!("{} {}", self.title(), self.description.unwrap_or_default()).to_lowercase();
        words.iter().all(|word| text.contains(word.as_str()))
    }
}

/// The catalogues that answer `query`, by their position in `all`, best first: those citing
/// the DOI it holds, if any, then those BM25 ranks, then those holding every word only inside
/// longer ones (`reconn`, being typed).
// simplify: the index is rebuilt for each query, fine for the few hundred public catalogues;
// past a few thousand, keep a `TextIndex` of the list and query it instead.
pub fn rank_catalogues(all: &[Catalogue<'_>], query: &str) -> Vec<usize> {
    let titles: Vec<String> = all.iter().map(Catalogue::title).collect();
    let index = TextIndex::new(all.iter().zip(&titles).map(|(catalogue, title)| Fields {
        title,
        body: catalogue.description.unwrap_or_default(),
    }));
    let ranked: Vec<usize> = index.search(query).into_iter().map(|(at, _)| at).collect();
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let typed: Vec<usize> = (0..all.len())
        .filter(|at| !ranked.contains(at) && all[*at].holds(&words))
        .collect();
    let found: Vec<usize> = ranked.into_iter().chain(typed).collect();
    let citing = doi(query).map_or_else(Vec::new, |doi| citing(all, doi));
    let (cited, others): (Vec<usize>, Vec<usize>) =
        found.into_iter().partition(|at| citing.contains(at));
    let unranked: Vec<usize> = citing
        .into_iter()
        .filter(|at| !cited.contains(at))
        .collect();
    cited.into_iter().chain(unranked).chain(others).collect()
}

/// The DOI a query holds, `10.3847/2041-8213/ab4429`, up to the next space: pasted alone or
/// inside a link to the paper.
fn doi(query: &str) -> Option<&str> {
    let start = query.find("10.")?;
    let doi = query[start..].split_whitespace().next()?;
    let (registrant, suffix) = doi.split_once('/')?;
    let numbered =
        registrant[3..].len() >= 4 && registrant[3..].chars().all(|c| c.is_ascii_digit());
    (numbered && !suffix.is_empty()).then_some(doi)
}

/// The catalogues whose text cites `doi`. A link may run past the DOI (`.../ab4429/pdf`): its
/// last parts are dropped one at a time until some catalogue cites what is left.
fn citing(all: &[Catalogue<'_>], doi: &str) -> Vec<usize> {
    let texts: Vec<String> = all
        .iter()
        .map(|catalogue| {
            format!(
                "{} {}",
                catalogue.title(),
                catalogue.description.unwrap_or_default()
            )
            .to_lowercase()
        })
        .collect();
    let mut doi = doi.to_lowercase();
    loop {
        let citing: Vec<usize> = (0..all.len())
            .filter(|at| texts[*at].contains(&doi))
            .collect();
        if !citing.is_empty() {
            return citing;
        }
        match doi.rsplit_once('/') {
            Some((shorter, _)) if shorter.contains('/') => doi = shorter.to_owned(),
            _ => return Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Catalogue, Fields, TextIndex, rank_catalogues};

    fn ranked(documents: &[(&str, &str)], query: &str) -> Vec<usize> {
        let index = TextIndex::new(documents.iter().map(|(title, body)| Fields { title, body }));
        index.search(query).into_iter().map(|(at, _)| at).collect()
    }

    #[test]
    fn a_word_in_the_title_beats_the_same_word_in_the_prose() {
        let documents = [
            // Unweighted, this short prose saying it twice would win.
            ("Wind magnetic clouds", "Solar Orbiter, Solar Orbiter."),
            (
                "Solar Orbiter reconnection exhausts",
                "Jets seen in the solar wind near the Sun during the first perihelion, \
                 measured by the plasma analyser and the magnetometer over one month.",
            ),
        ];
        assert_eq!(ranked(&documents, "solar orbiter"), [1, 0]);
    }

    #[test]
    fn a_rare_word_outweighs_a_common_one() {
        let documents = [
            ("Solar wind ICMEs", "Solar wind solar wind."),
            ("Reconnection exhausts", "In the solar wind."),
            ("Shocks", "Solar wind shocks."),
        ];
        assert_eq!(ranked(&documents, "solar wind reconnection")[0], 1);
    }

    #[test]
    fn a_pasted_doi_finds_its_paper() {
        let documents = [
            (
                "Saturn reconnection",
                "Cite: Guo et al. (2019), https://doi.org/10.3847/2041-8213/ab4429.",
            ),
            (
                "PSP current sheet",
                "Cite: Fargette et al. (2026), https://doi.org/10.3847/1538-4357/ae2601.",
            ),
        ];
        assert_eq!(ranked(&documents, "10.3847/2041-8213/ab4429")[0], 0);
    }

    #[test]
    fn a_singular_finds_the_plural_and_the_other_way_round() {
        let documents = [
            ("ICMEs at STEREO", ""),
            ("Reconnection exhaust", ""),
            ("Mass", ""),
        ];
        assert_eq!(ranked(&documents, "icme"), [0]);
        assert_eq!(ranked(&documents, "exhausts"), [1]);
        assert_eq!(ranked(&documents, "mass"), [2], "a double s stays");
    }

    #[test]
    fn an_acronym_finds_the_name_written_out_and_the_other_way_round() {
        let documents = [
            ("Solar Orbiter exhausts", ""),
            ("SolO EUI events", ""),
            ("Coronal mass ejections", ""),
            ("Solar wind", ""),
            ("Solar flares", ""),
            ("Solar radio bursts", ""),
        ];
        assert_eq!(sorted(ranked(&documents, "SolO")), [0, 1]);
        let written_out = ranked(&documents, "solar orbiter");
        assert_eq!(
            sorted(written_out[..2].to_vec()),
            [0, 1],
            "then those only solar"
        );
        assert_eq!(ranked(&documents, "CME"), [2]);
        assert!(
            ranked(&documents, "solar").contains(&0),
            "the words written out still count"
        );
    }

    fn sorted(mut found: Vec<usize>) -> Vec<usize> {
        found.sort_unstable();
        found
    }

    #[test]
    fn a_doi_or_a_paper_link_puts_the_catalogues_citing_it_first() {
        let all = [
            Catalogue {
                name: "Saturn reconnection",
                description: Some("Cite: https://doi.org/10.3847/2041-8213/ab4429 (Table 1)."),
                ..Catalogue::default()
            },
            Catalogue {
                name: "Article pdf from iopscience",
                description: Some(
                    "https://iopscience.iop.org/article/10.3847/2041-8213/pdf, \
                     https://iopscience.iop.org/article/10.3847/1538-4357/ae2601/pdf",
                ),
                ..Catalogue::default()
            },
        ];
        let link = "https://iopscience.iop.org/article/10.3847/2041-8213/ab4429/pdf";
        assert_eq!(rank_catalogues(&all, link)[0], 0);
        assert_eq!(rank_catalogues(&all, "10.3847/2041-8213/AB4429")[0], 0);
    }

    #[test]
    fn case_and_punctuation_do_not_matter_and_unknown_words_find_nothing() {
        let documents = [("ICME list (Jian)", "")];
        assert_eq!(ranked(&documents, "icme, JIAN"), [0]);
        assert!(ranked(&documents, "magnetopause").is_empty());
    }
}
