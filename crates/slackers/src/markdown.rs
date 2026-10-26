//! Markdown, which Drafts are written in, and Slack's own markup, `mrkdwn`, which Slack reads: one into the other,
//! and Messages back as Markdown to edit them.
//!
//! The two write `_italic_`, `` `code` ``, code blocks and `> quotes` alike. `*bold*` stays bold, as in Slack,
//! rather than italic as in Markdown. What Slack has no way to write, such as a heading or a code block's language,
//! doesn't come back.

use slackers_model::Style;
use slackers_slack::Part;

/// `text`, written in Markdown, as Slack's markup: `**bold**` as `*bold*`, `~~struck~~` as `~struck~`, headings
/// as bold lines, code blocks without their language, and as links: `[label](url)`, an image's `![alt](url)`,
/// `<url>`, and URLs starting with `http://` or `https://`, as GitHub's Markdown links them. Nothing changes in
/// code, which runs from a backtick to the next, fences included.
pub(crate) fn to_slack(text: &str) -> Vec<Part> {
    inlines(&blocks(text))
}

/// `text` in `style`, as Markdown writes it: between `**` for bold, `_` for italic, `~~` for strikethrough and
/// `` ` `` for code. Spaces at either end stay outside the markers, which only count against words.
pub(crate) fn styled(text: &str, style: Style) -> String {
    let markers: String = [
        (Style::BOLD, "**"),
        (Style::ITALIC, "_"),
        (Style::STRIKE, "~~"),
        (Style::CODE, "`"),
    ]
    .into_iter()
    .filter(|(marker, _)| style.contains(*marker))
    .map(|(_, mark)| mark)
    .collect();
    let words = text.trim();
    if markers.is_empty() || words.is_empty() {
        return text.to_owned();
    }
    let (start, end) = (text.len() - text.trim_start().len(), text.trim_end().len());
    let closing: String = markers.chars().rev().collect();
    format!(
        "{}{markers}{words}{closing}{}",
        &text[..start],
        &text[end..]
    )
}

/// A link to `url`, showing `label` if it has one, as Markdown writes it: `[label](url)`; or, without a label or
/// where Markdown wouldn't read it back as the link's, the URL alone, between `<` and `>` unless it's one that
/// Markdown links by itself.
pub(crate) fn link(url: &str, label: Option<&str>) -> String {
    if let Some(label) = label.filter(|label| !label.is_empty() && *label != url) {
        let written = format!("[{label}]({url})");
        let link = Part::Link {
            url: url.to_owned(),
            label: label.to_owned(),
        };
        if labelled(&written) == Some((link, written.len())) {
            return written;
        }
    }
    if bare_url(url) == Some(url) {
        url.to_owned()
    } else {
        format!("<{url}>")
    }
}

/// `text` with its headings as bold lines, and without its code blocks' language.
fn blocks(text: &str) -> String {
    let mut in_code = false;
    let lines: Vec<String> = text
        .split('\n')
        .map(|line| {
            let written = if in_code {
                line.to_owned()
            } else if line.strip_prefix("```").is_some_and(is_language) {
                "```".to_owned()
            } else {
                heading(line).map_or_else(|| line.to_owned(), |title| format!("*{title}*"))
            };
            in_code ^= written.matches('`').count() % 2 == 1;
            written
        })
        .collect();
    lines.join("\n")
}

/// Whether `word`, after a code block's opening fence, names its language, as `rust` or `c++` do.
fn is_language(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '+' | '-' | '#' | '.' | '_'))
}

/// `line`'s title, if it's a heading: one to six `#`, a space, and the title, which isn't blank. `#`s closing it,
/// after a space, aren't part of it.
fn heading(line: &str) -> Option<&str> {
    let hashes = line.len() - line.trim_start_matches('#').len();
    let title = line[hashes..].strip_prefix(' ')?.trim();
    let unclosed = title.trim_end_matches('#');
    let title = if unclosed.ends_with(' ') {
        unclosed.trim_end()
    } else {
        title
    };
    ((1..=6).contains(&hashes) && !title.is_empty()).then_some(title)
}

/// `text` as Slack's text and links, after [`blocks`].
fn inlines(text: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut plain = String::new();
    let mut in_code = false;
    // Where each `**` or `~~` that closes one opened before starts.
    let mut closers = Vec::new();
    let mut at = 0;
    while let Some(c) = text[at..].chars().next() {
        let rest = &text[at..];
        if c == '`' {
            in_code = !in_code;
        } else if !in_code && (rest.starts_with("**") || rest.starts_with("~~")) {
            if let Some(index) = closers.iter().position(|&closer| closer == at) {
                closers.swap_remove(index);
                plain.push(c);
            } else if let Some(closer) = closer(text, at) {
                closers.push(closer);
                plain.push(c);
            } else {
                plain.push_str(&rest[..2]);
            }
            at += 2;
            continue;
        } else if !in_code && let Some((link, length)) = link_at(text, at) {
            if !plain.is_empty() {
                parts.push(Part::Text(std::mem::take(&mut plain)));
            }
            parts.push(link);
            at += length;
            continue;
        }
        plain.push(c);
        at += c.len_utf8();
    }
    if !plain.is_empty() {
        parts.push(Part::Text(plain));
    }
    parts
}

/// Where the `**` or `~~` closing the one at `at` in `text` starts: later on the same line, outside code, right
/// after something other than a space. The one at `at` opens only right before something other than a space.
fn closer(text: &str, at: usize) -> Option<usize> {
    let marker = &text[at..at + 2];
    let after = &text[at + 2..];
    let line = &after[..after.find('\n').unwrap_or(after.len())];
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let mut in_code = false;
    line.char_indices().find_map(|(offset, c)| {
        if c == '`' {
            in_code = !in_code;
        }
        (!in_code
            && offset > 0
            && line[offset..].starts_with(marker)
            && !line[..offset].ends_with(char::is_whitespace))
        .then_some(at + 2 + offset)
    })
}

/// The link at `at` in `text`, and its length in bytes: `[label](url)`; `![alt](url)`, an image, linked to;
/// `<url>`; or, starting a word, a URL as [`bare_url`] finds it.
fn link_at(text: &str, at: usize) -> Option<(Part, usize)> {
    let rest = &text[at..];
    let image = usize::from(rest.starts_with("!["));
    if let Some((link, length)) = labelled(&rest[image..]) {
        return Some((link, image + length));
    }
    let starts_word = text[..at]
        .chars()
        .next_back()
        .is_none_or(|before| before.is_whitespace() || matches!(before, '*' | '_' | '~' | '('));
    let (url, length) = match rest
        .strip_prefix('<')
        .and_then(|inner| inner.split_once('>'))
    {
        Some((url, _)) if has_scheme(url) && !url.contains(['<', '|']) => (url, url.len() + 2),
        _ if starts_word => bare_url(rest).map(|url| (url, url.len()))?,
        _ => return None,
    };
    let link = Part::Link {
        url: url.to_owned(),
        label: url.to_owned(),
    };
    Some((link, length))
}

/// The Markdown link `text` starts with, and its length in bytes: `[label](url)`, its label on one line and not
/// blank, its URL with a scheme and no whitespace. Anything else isn't one.
fn labelled(text: &str) -> Option<(Part, usize)> {
    let (label, rest) = text.strip_prefix('[')?.split_once("](")?;
    if label.trim().is_empty() || label.contains(['[', ']', '\n']) {
        return None;
    }
    // Parentheses may pair up inside the URL, as in Wikipedia's.
    let mut depth = 0;
    let mut end = None;
    for (at, c) in rest.char_indices() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => {
                end = Some(at);
                break;
            }
            ')' => depth -= 1,
            _ => {}
        }
    }
    let url = &rest[..end?];
    let link = Part::Link {
        url: url.to_owned(),
        label: label.to_owned(),
    };
    has_scheme(url).then_some((
        link,
        "[".len() + label.len() + "](".len() + url.len() + ")".len(),
    ))
}

/// The URL `text` starts with, if it starts with `http://` or `https://`: up to a space or a `<`, but for
/// punctuation at its end, and a `)` there that closes no `(` in it.
fn bare_url(text: &str) -> Option<&str> {
    let host = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))?;
    let end = text
        .find(|c: char| c.is_whitespace() || c == '<')
        .unwrap_or(text.len());
    let mut url = &text[..end];
    loop {
        let trimmed =
            url.trim_end_matches(['?', '!', '.', ',', ':', ';', '*', '_', '~', '\'', '"']);
        let trimmed = match trimmed.strip_suffix(')') {
            Some(unclosed) if trimmed.matches(')').count() > trimmed.matches('(').count() => {
                unclosed
            }
            _ => trimmed,
        };
        if trimmed.len() == url.len() {
            break;
        }
        url = trimmed;
    }
    (url.len() > text.len() - host.len()).then_some(url)
}

/// Whether `url` starts with a scheme, as `https:` and `mailto:` do, and has no whitespace.
fn has_scheme(url: &str) -> bool {
    url.split_once(':').is_some_and(|(scheme, after)| {
        scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            && !after.is_empty()
            && !url.contains(char::is_whitespace)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str) -> Part {
        Part::Text(text.into())
    }

    fn link(url: &str, label: &str) -> Part {
        Part::Link {
            url: url.into(),
            label: label.into(),
        }
    }

    fn unchanged(as_typed: &str) {
        assert_eq!(to_slack(as_typed), [text(as_typed)], "{as_typed}");
    }

    #[test]
    fn markdown_links_are_links() {
        assert_eq!(
            to_slack("Hey, look [at this](https://github.com), it's great"),
            [
                text("Hey, look "),
                link("https://github.com", "at this"),
                text(", it's great"),
            ]
        );
        let wiki = "https://en.wikipedia.org/wiki/Rust_(programming_language)";
        assert_eq!(
            to_slack(&format!("[Rust]({wiki}).\n[a] ![me](mailto:a@b.c)")),
            [
                link(wiki, "Rust"),
                text(".\n[a] "),
                link("mailto:a@b.c", "me"),
            ]
        );
        // Not http URLs, which would be links by themselves.
        for as_typed in [
            "[no scheme](github.com)",
            "[a time](2:30)",
            "[only a scheme](mailto:)",
            "[spaced](mailto:a b)",
            "[ ](mailto:a@b.c)",
            "[unclosed](mailto:a@b.c",
            "[two\nlines](mailto:a@b.c)",
            "`[code](https://a.com)`",
            "```\n[fenced](https://a.com)\n```",
        ] {
            unchanged(as_typed);
        }
    }

    #[test]
    fn urls_are_links_as_github_finds_them() {
        assert_eq!(
            to_slack(
                "see <https://a.com/x>, https://b.com/y?q=1. (http://c.com/(z)) _https://d.com_"
            ),
            [
                text("see "),
                link("https://a.com/x", "https://a.com/x"),
                text(", "),
                link("https://b.com/y?q=1", "https://b.com/y?q=1"),
                text(". ("),
                link("http://c.com/(z)", "http://c.com/(z)"),
                text(") _"),
                link("https://d.com", "https://d.com"),
                text("_"),
            ]
        );
        for as_typed in [
            "https://",
            "xhttps://a.com",
            "`https://a.com`",
            "<a.com>",
            "<https://a.com|label>",
            "<@U1>",
        ] {
            unchanged(as_typed);
        }
    }

    #[test]
    fn double_markers_are_slacks_single_ones() {
        assert_eq!(
            to_slack("**bold** and ~~struck~~, **`code` too**, *still bold*"),
            [text("*bold* and ~struck~, *`code` too*, *still bold*")]
        );
        assert_eq!(
            to_slack("**[a](https://a.com)**"),
            [text("*"), link("https://a.com", "a"), text("*")]
        );
        for as_typed in [
            "f(**kwargs)",
            "2 ** 3 ** 4",
            "** spaced**",
            "**spaced **",
            "****",
            "**two\nlines**",
            "`**code**`",
            "**`a**` b",
        ] {
            unchanged(as_typed);
        }
    }

    #[test]
    fn headings_are_bold_lines() {
        assert_eq!(
            to_slack("# Title\nbody\n### Learn C#\n## Closed ##"),
            [text("*Title*\nbody\n*Learn C#*\n*Closed*")]
        );
        for as_typed in ["#general", "# ", "####### seven", "```\n# a comment\n```"] {
            unchanged(as_typed);
        }
    }

    #[test]
    fn code_blocks_lose_their_language() {
        assert_eq!(
            to_slack("```rust\nlet x = 1;\n```\n```c++\n```"),
            [text("```\nlet x = 1;\n```\n```\n```")]
        );
        for as_typed in ["```cargo build\n```", "```inline```", "```\nrust\n```"] {
            unchanged(as_typed);
        }
    }

    #[test]
    fn styles_write_as_markers_around_the_words_only() {
        assert_eq!(styled("plain ", Style::PLAIN), "plain ");
        assert_eq!(styled(" bold ", Style::BOLD), " **bold** ");
        assert_eq!(
            styled(
                "all of it",
                Style::BOLD | Style::ITALIC | Style::STRIKE | Style::CODE
            ),
            "**_~~`all of it`~~_**"
        );
        assert_eq!(styled("  ", Style::BOLD), "  ", "no words, no markers");
    }

    #[test]
    fn what_is_written_reads_back_as_slack_had_it() {
        let all = Style::BOLD | Style::ITALIC | Style::STRIKE | Style::CODE;
        assert_eq!(
            to_slack(&styled("all of it", all)),
            [text("*_~`all of it`~_*")]
        );
        for (url, label) in [
            ("https://a.com/(b)", Some("the docs")),
            ("https://a.com", None),
            ("mailto:a@b.c", None),
        ] {
            let written = super::link(url, label);
            assert_eq!(
                to_slack(&written),
                [link(url, label.unwrap_or(url))],
                "{written}"
            );
        }
        assert_eq!(
            super::link("https://a.com", Some("[odd]")),
            "https://a.com",
            "a label Markdown can't write goes"
        );
        assert_eq!(
            super::link("https://a.com", Some("https://a.com")),
            "https://a.com"
        );
    }
}
