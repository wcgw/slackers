//! Finding people, emoji and slash commands by the start of a name. Conversations are found by the kit's picker,
//! fzf-style.

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
}
