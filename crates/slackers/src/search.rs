//! Finding Conversations by name, fzf-style, and people by the start of a name.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Ranks names against queries, keeping the matcher's scratch space between calls.
#[derive(Debug)]
pub(crate) struct Finder {
    matcher: Matcher,
    buffer: Vec<char>,
}

/// A name that matches a query, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hit {
    /// The name's position among those ranked.
    pub(crate) index: usize,
    /// Which of its characters matched, by char index, ascending.
    pub(crate) positions: Vec<usize>,
}

impl Finder {
    pub(crate) fn new() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
            buffer: Vec::new(),
        }
    }

    /// The `names` that match `query`, best first; equally good ones keep their order. An empty query matches every
    /// name. The syntax is fzf's: every space-separated term must match, fuzzily unless it starts with `'` (exact)
    /// or `^` (prefix); lower case matches either case.
    pub(crate) fn rank<'a>(
        &mut self,
        query: &str,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Vec<Hit> {
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut scored: Vec<(u32, Hit)> = names
            .into_iter()
            .enumerate()
            .filter_map(|(index, name)| {
                let mut indices = Vec::new();
                let haystack = Utf32Str::new(name, &mut self.buffer);
                let score = pattern.indices(haystack, &mut self.matcher, &mut indices)?;
                // Several terms can match the same character; positions come back in no particular order.
                indices.sort_unstable();
                indices.dedup();
                let positions = indices.into_iter().map(|i| i as usize).collect();
                Some((score, Hit { index, positions }))
            })
            .collect();
        // Stable: equally good names keep their order.
        scored.sort_by(|(a, _), (b, _)| b.cmp(a));
        scored.into_iter().map(|(_, hit)| hit).collect()
    }
}

/// Where `query` starts a word of `name`, ignoring case: which characters of `name` it matches, by char index,
/// ascending, at the first word it starts; `None` when it starts none. A word starts `name`, or follows a space or
/// one of `.-_'(`. An empty query starts every name, at no characters.
pub(crate) fn word_start(name: &str, query: &str) -> Option<Vec<usize>> {
    let chars: Vec<char> = name.chars().collect();
    let length = query.chars().count();
    let same = |a: char, b: char| a == b || a.to_lowercase().eq(b.to_lowercase());
    (0..chars.len().max(1))
        .filter(|&at| at == 0 || chars[at - 1].is_whitespace() || ".-_'(".contains(chars[at - 1]))
        .find(|&at| {
            at + length <= chars.len()
                && chars[at..]
                    .iter()
                    .zip(query.chars())
                    .all(|(&a, b)| same(a, b))
        })
        .map(|at| (at..at + length).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_matches_where_the_query_starts_one_of_its_words() {
        assert_eq!(word_start("Ada Lovelace", "ad"), Some(vec![0, 1]));
        assert_eq!(word_start("Ada Lovelace", "LOVE"), Some(vec![4, 5, 6, 7]));
        assert_eq!(word_start("mary-jo.smith", "jo"), Some(vec![5, 6]));
        assert_eq!(word_start("Zoë", "zoë"), Some(vec![0, 1, 2]));
        assert_eq!(word_start("Ada", ""), Some(vec![]));
        assert_eq!(word_start("", ""), Some(vec![]));
        for (name, query) in [
            ("Benjamin Ross", "jo"),
            ("Ada", "da"),
            ("Ada", "adam"),
            ("", "a"),
        ] {
            assert_eq!(word_start(name, query), None, "{name} / {query}");
        }
    }

    const NAMES: [&str; 5] = [
        "general",
        "kuadrant-dev",
        "random",
        "Kuadrant Bot",
        "dev-kubernetes",
    ];

    fn ranked(query: &str) -> Vec<&'static str> {
        Finder::new()
            .rank(query, NAMES)
            .into_iter()
            .map(|hit| NAMES[hit.index])
            .collect()
    }

    #[test]
    fn an_empty_query_keeps_everything_in_order() {
        assert_eq!(ranked(""), NAMES);
    }

    #[test]
    fn fuzzy_matches_rank_the_tightest_first() {
        assert_eq!(ranked("kdev"), ["kuadrant-dev"]);
        assert_eq!(
            ranked("dev")[0],
            "dev-kubernetes",
            "a match at the start beats one at the end"
        );
        assert!(ranked("xyz").is_empty());
    }

    #[test]
    fn lower_case_ignores_case_but_upper_case_doesnt() {
        assert_eq!(ranked("bot"), ["Kuadrant Bot"]);
        assert!(ranked("BOT").is_empty());
    }

    #[test]
    fn several_terms_must_all_match() {
        assert_eq!(ranked("kua bot"), ["Kuadrant Bot"]);
        assert_eq!(ranked("^dev"), ["dev-kubernetes"]);
    }

    #[test]
    fn positions_point_at_the_matched_characters() {
        let hits = Finder::new().rank("gnl", ["general"]);
        assert_eq!(
            hits,
            [Hit {
                index: 0,
                positions: vec![0, 2, 6]
            }]
        );
    }
}
