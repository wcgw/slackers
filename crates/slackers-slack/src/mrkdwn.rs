//! Slack's `mrkdwn` text format, which Messages without rich text blocks (most bots, older Messages) use.
//!
//! Reference: <https://docs.slack.dev/messaging/formatting-message-text>

use slackers_model::{Block, Broadcast, ConversationId, Emoji, Inline, RichText, Style, UserId};

use crate::session::Part;

/// Parses `mrkdwn` text into [`RichText`]: code blocks, `>` quotes, `*bold*`-style markers, `:emoji:`, and
/// `<…>` mentions and links. Anything malformed stays as plain text.
pub(crate) fn parse(text: &str) -> RichText {
    let mut blocks = Vec::new();
    // Splitting at fences makes every odd part a code block; an unclosed fence runs to the end.
    for (index, part) in text.split("```").enumerate() {
        if index % 2 == 1 {
            blocks.push(Block::Preformatted(decode_entities(
                part.trim_matches('\n'),
            )));
            continue;
        }
        let mut lines = part.split('\n').peekable();
        while lines.peek().is_some() {
            let is_quote = |line: &&str| line.starts_with("&gt;");
            let quoted = lines.peek().is_some_and(is_quote);
            let mut run = Vec::new();
            while let Some(line) = lines.next_if(|line| is_quote(line) == quoted) {
                run.push(if quoted {
                    line.trim_start_matches("&gt;").trim_start_matches(' ')
                } else {
                    line
                });
            }
            let joined = run.join("\n");
            let trimmed = joined.trim_matches('\n');
            if trimmed.trim().is_empty() {
                continue;
            }
            let inlines = parse_inlines(trimmed);
            blocks.push(if quoted {
                Block::Quote(inlines)
            } else {
                Block::Paragraph(inlines)
            });
        }
    }
    RichText { blocks }
}

/// Parses one paragraph's worth of inline `mrkdwn`.
fn parse_inlines(text: &str) -> Vec<Inline> {
    let mut out = Vec::new();
    push_inlines(text, Style::PLAIN, &mut out);
    out
}

fn push_inlines(text: &str, style: Style, out: &mut Vec<Inline>) {
    let mut plain = String::new();
    let mut previous: Option<char> = None;
    let mut rest = text;
    let flush = |plain: &mut String, out: &mut Vec<Inline>| {
        if !plain.is_empty() {
            out.push(Inline::Text {
                text: decode_entities(plain),
                style,
            });
            plain.clear();
        }
    };
    while let Some(c) = rest.chars().next() {
        let at_word_start = previous.is_none_or(|p| !p.is_alphanumeric());
        match c {
            '<' => {
                if let Some(end) = rest.find('>') {
                    flush(&mut plain, out);
                    out.push(angle_token(&rest[1..end], style));
                    rest = &rest[end + 1..];
                    previous = Some('>');
                    continue;
                }
            }
            ':' => {
                if let Some((emoji, length)) = emoji_at(rest) {
                    flush(&mut plain, out);
                    out.push(Inline::Emoji(emoji));
                    rest = &rest[length..];
                    previous = Some(':');
                    continue;
                }
            }
            '*' | '_' | '~' | '`' if at_word_start => {
                if let Some(end) = closing_marker(rest, c) {
                    flush(&mut plain, out);
                    let inner = &rest[1..end];
                    match c {
                        '`' => out.push(Inline::Text {
                            text: decode_entities(inner),
                            style: style | Style::CODE,
                        }),
                        '*' => push_inlines(inner, style | Style::BOLD, out),
                        '_' => push_inlines(inner, style | Style::ITALIC, out),
                        _ => push_inlines(inner, style | Style::STRIKE, out),
                    }
                    rest = &rest[end + 1..];
                    previous = Some(c);
                    continue;
                }
            }
            _ => {}
        }
        plain.push(c);
        previous = Some(c);
        rest = &rest[c.len_utf8()..];
    }
    flush(&mut plain, out);
}

/// What's between `<` and `>`: a mention, a broadcast, a date, or a link, each optionally followed by `|label`.
fn angle_token(token: &str, style: Style) -> Inline {
    let (target, label) = match token.split_once('|') {
        Some((target, label)) => (target, Some(label)),
        None => (token, None),
    };
    if let Some(user) = target.strip_prefix('@') {
        return Inline::User(UserId::new(user));
    }
    if let Some(channel) = target.strip_prefix('#') {
        return Inline::Channel(ConversationId::new(channel));
    }
    if let Some(command) = target.strip_prefix('!') {
        return match command {
            "here" => Inline::Broadcast(Broadcast::Here),
            "channel" => Inline::Broadcast(Broadcast::Channel),
            "everyone" => Inline::Broadcast(Broadcast::Everyone),
            _ => match command.strip_prefix("subteam^") {
                Some(group) => Inline::UserGroup(group.to_owned()),
                // `<!date^…|fallback>` and anything newer: show what Slack offers as text.
                None => Inline::Text {
                    text: decode_entities(label.unwrap_or(command)),
                    style,
                },
            },
        };
    }
    Inline::Link {
        url: decode_entities(target),
        label: label.map(decode_entities),
    }
}

/// The emoji at the start of `text` (`:name:`, maybe followed by `:skin-tone-N:`), and how many bytes it takes.
fn emoji_at(text: &str) -> Option<(Emoji, usize)> {
    let end = 1 + text[1..].find(':')?;
    let name = &text[1..end];
    let is_name_char = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || "_+-'".contains(c);
    if name.is_empty() || name.len() > 100 || !name.chars().all(is_name_char) {
        return None;
    }
    let mut emoji = Emoji::named(name);
    let mut length = end + 1;
    // A skin tone follows as its own `:skin-tone-N:`, as in `:thumbsup::skin-tone-2:`.
    if let Some(tone) = text[length..].strip_prefix(":skin-tone-")
        && let Some(digit) = tone.chars().next().and_then(|d| d.to_digit(10))
        && (2..=6).contains(&digit)
        && tone[1..].starts_with(':')
    {
        emoji.skin_tone = u8::try_from(digit).ok();
        length += ":skin-tone-N:".len();
    }
    Some((emoji, length))
}

/// Where the closing `marker` of a formatting run starting at `text[0]` is: on the same line, after at least one
/// character, not preceded by whitespace, and not followed by a letter or digit.
fn closing_marker(text: &str, marker: char) -> Option<usize> {
    let line_end = text.find('\n').unwrap_or(text.len());
    let body = &text[1..line_end];
    if body.starts_with(char::is_whitespace) {
        return None;
    }
    body.char_indices().find_map(|(at, c)| {
        let after = body[at + c.len_utf8()..].chars().next();
        let before = body[..at].chars().next_back();
        (c == marker
            && at > 0
            && before.is_some_and(|b| !b.is_whitespace())
            && after.is_none_or(|a| !a.is_alphanumeric()))
        .then_some(at + 1)
    })
}

/// `parts` as `mrkdwn`: text with its `&`, `<` and `>` escaped, and mentions as Slack writes them.
pub(crate) fn write(parts: &[Part]) -> String {
    let mut written = String::new();
    for part in parts {
        match part {
            Part::Text(text) => written.push_str(&escape(text)),
            Part::User(id) => written.push_str(&format!("<@{id}>")),
            Part::Broadcast(broadcast) => written.push_str(match broadcast {
                Broadcast::Here => "<!here>",
                Broadcast::Channel => "<!channel>",
                Broadcast::Everyone => "<!everyone>",
            }),
            Part::UserGroup(id) => written.push_str(&format!("<!subteam^{id}>")),
            Part::Channel(id) => written.push_str(&format!("<#{id}>")),
            Part::Link { url, label } => {
                // Encoded as in any URL, `|`, `<` and `>` can't end the link early.
                let url = url
                    .replace('|', "%7C")
                    .replace('<', "%3C")
                    .replace('>', "%3E");
                written.push_str(&format!("<{url}|{}>", escape(label)));
            }
        }
    }
    written
}

/// Escapes `&`, `<` and `>`, which `mrkdwn` reserves for its markup, so that text means what it says.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Undoes Slack's escaping of `&`, `<` and `>`.
pub(crate) fn decode_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str) -> Inline {
        Inline::Text {
            text: text.into(),
            style: Style::PLAIN,
        }
    }

    fn styled(text: &str, style: Style) -> Inline {
        Inline::Text {
            text: text.into(),
            style,
        }
    }

    fn paragraph(input: &str) -> Vec<Inline> {
        match parse(input).blocks.as_slice() {
            [Block::Paragraph(inlines)] => inlines.clone(),
            other => panic!("expected one paragraph, got {other:?}"),
        }
    }

    #[test]
    fn mentions_links_and_broadcasts() {
        assert_eq!(
            paragraph("hi <@U1>, see <#C2|general> and <https://a.b/?x=1&amp;y=2|docs> <!here>"),
            [
                text("hi "),
                Inline::User(UserId::new("U1")),
                text(", see "),
                Inline::Channel(ConversationId::new("C2")),
                text(" and "),
                Inline::Link {
                    url: "https://a.b/?x=1&y=2".into(),
                    label: Some("docs".into())
                },
                text(" "),
                Inline::Broadcast(Broadcast::Here),
            ]
        );
    }

    #[test]
    fn emoji_with_and_without_skin_tone() {
        let mut thumbs = Emoji::named("thumbsup");
        thumbs.skin_tone = Some(3);
        assert_eq!(
            paragraph(":wave: hi :thumbsup::skin-tone-3: at 10:30"),
            [
                Inline::Emoji(Emoji::named("wave")),
                text(" hi "),
                Inline::Emoji(thumbs),
                text(" at 10:30")
            ]
        );
    }

    #[test]
    fn styles_apply_at_word_boundaries_only() {
        assert_eq!(
            paragraph("*bold _both_* and snake_case_name and `a*b`"),
            [
                styled("bold ", Style::BOLD),
                styled("both", Style::BOLD | Style::ITALIC),
                text(" and snake_case_name and "),
                styled("a*b", Style::CODE),
            ]
        );
    }

    #[test]
    fn code_blocks_and_quotes_become_blocks() {
        let parsed = parse("before\n```\nlet x = 1 &lt; 2;\n```\n&gt; quoted\n&gt; more\nafter");
        assert_eq!(
            parsed.blocks,
            [
                Block::Paragraph(vec![text("before")]),
                Block::Preformatted("let x = 1 < 2;".into()),
                Block::Quote(vec![text("quoted\nmore")]),
                Block::Paragraph(vec![text("after")]),
            ]
        );
    }

    #[test]
    fn written_mentions_read_back_as_mentions() {
        let parts = [
            Part::Text("<b> & ".into()),
            Part::User(UserId::new("U1")),
            Part::Text(", ".into()),
            Part::Broadcast(Broadcast::Here),
        ];
        assert_eq!(write(&parts), "&lt;b&gt; &amp; <@U1>, <!here>");
        assert_eq!(
            paragraph(&write(&parts)),
            [
                text("<b> & "),
                Inline::User(UserId::new("U1")),
                text(", "),
                Inline::Broadcast(Broadcast::Here),
            ]
        );
    }

    #[test]
    fn written_links_channels_and_groups_read_back_as_such() {
        let link = Inline::Link {
            url: "https://example.com/?a=1".into(),
            label: Some("the <docs>".into()),
        };
        let parts = [
            Part::Channel(ConversationId::new("C2")),
            Part::Text(" ".into()),
            Part::UserGroup("S1".into()),
            Part::Text(" ".into()),
            Part::Link {
                url: "https://example.com/?a=1".into(),
                label: "the <docs>".into(),
            },
        ];
        assert_eq!(
            paragraph(&write(&parts)),
            [
                Inline::Channel(ConversationId::new("C2")),
                text(" "),
                Inline::UserGroup("S1".into()),
                text(" "),
                link,
            ]
        );
    }

    #[test]
    fn a_written_links_url_cant_end_it_early() {
        let parts = [Part::Link {
            url: "https://example.com/a|b>c<d".into(),
            label: "x".into(),
        }];
        assert_eq!(
            paragraph(&write(&parts)),
            [Inline::Link {
                url: "https://example.com/a%7Cb%3Ec%3Cd".into(),
                label: Some("x".into()),
            }]
        );
    }

    #[test]
    fn escaping_round_trips() {
        let text = "a < b && c > d";
        assert_eq!(escape(text), "a &lt; b &amp;&amp; c &gt; d");
        assert_eq!(decode_entities(&escape(text)), text);
    }

    #[test]
    fn malformed_markup_stays_text() {
        assert_eq!(
            paragraph("a < b and *not bold"),
            [text("a < b and *not bold")]
        );
    }
}
