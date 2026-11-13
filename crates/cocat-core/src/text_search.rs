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
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(|word| singular(&word.to_lowercase()).to_owned())
            .collect()
    }
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

#[cfg(test)]
mod tests {
    use super::{Fields, TextIndex};

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
    fn case_and_punctuation_do_not_matter_and_unknown_words_find_nothing() {
        let documents = [("ICME list (Jian)", "")];
        assert_eq!(ranked(&documents, "icme, JIAN"), [0]);
        assert!(ranked(&documents, "magnetopause").is_empty());
    }
}
