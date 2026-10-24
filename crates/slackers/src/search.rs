//! Finding Conversations by name, fzf-style.

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

#[cfg(test)]
mod tests {
    use super::*;

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
