//! Turning snapshot strings into Markdown that means what the string said.
//!
//! Three encodings live here and they are deliberately not interchangeable:
//!
//! - the **text source anchor** ([`source_anchor`]) is issue #7's grammar,
//!   copied verbatim into `.rr/SYMBOLS.md` so a value pasted into `rr query`
//!   works. It is an identity, not a URL.
//! - the **Markdown destination** ([`destination`]) is RFC 3986 percent-encoding
//!   for the inside of a `<...>` link target. It is navigation syntax, not an
//!   identity.
//! - **display escaping** ([`escape_label`], [`code_span`]) keeps a name that
//!   contains Markdown punctuation from being read as Markdown.
//!
//! Conflating the first two is the specific mistake this module exists to make
//! hard: they encode different character sets, and a value round-tripped
//! through the wrong one is silently wrong rather than loudly broken.

use super::{TextError, TextResult};
use crate::path::RelPath;

/// The issue #7 source anchor for a path, optionally naming a symbol.
///
/// Delegates to the one formatter issue #7 already owns. A second
/// implementation here would be a second grammar the moment either changed.
pub(crate) fn source_anchor(path: &str, symbol: Option<&str>) -> String {
    crate::render::encode_anchor(path, symbol)
}

/// The anchor for a test file that has no named test definition.
///
/// Issue #7's grammar has no "whole file" form, so the line-one anchor stands
/// in for it. `L1` is data in the symbol position, not a special case in the
/// parser, which is why it needs no encoder of its own.
pub(crate) fn file_anchor(path: &str) -> String {
    source_anchor(path, Some("L1"))
}

/// A Markdown link destination for the inside of `<...>`.
///
/// Each component is percent-encoded per RFC 3986 unreserved rules and the two
/// are joined by one structural `#`. `/` survives so a reader still sees a
/// path; every other non-unreserved byte is escaped, which is what keeps
/// spaces, parentheses, and `>` from ending the destination early.
pub(crate) fn destination(path: &str, symbol: Option<&str>) -> String {
    let mut out = String::with_capacity(path.len());
    encode_destination_component(path, &mut out);
    if let Some(symbol) = symbol {
        out.push('#');
        encode_destination_component(symbol, &mut out);
    }
    out
}

/// Whether this byte may appear literally in a destination.
///
/// `/` is here in addition to RFC 3986's unreserved set because a path
/// separator that reads as `%2F` helps nobody. Every other reserved character
/// is encoded: this crate never emits a destination with query or fragment
/// syntax of its own beyond the single structural `#` added above.
const fn is_destination_literal(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/')
}

fn encode_destination_component(value: &str, out: &mut String) {
    for byte in value.bytes() {
        if is_destination_literal(byte) {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(upper_hex(byte >> 4));
            out.push(upper_hex(byte & 0x0F));
        }
    }
}

const fn upper_hex(nibble: u8) -> char {
    (match nibble {
        0..=9 => b'0' + nibble,
        _ => b'A' + (nibble - 10),
    }) as char
}

/// Decodes a Markdown destination back into a path and optional symbol.
///
/// Rejects rather than repairs: a lowercase escape, a truncated escape, an
/// escape that decodes to something which is not UTF-8, and a path `RelPath`
/// refuses are all errors. This runs against files a human may have edited, so
/// "close enough" would mean accepting a link that points somewhere else.
///
/// # Errors
/// Returns [`TextError::Destination`] for any non-canonical spelling.
pub(crate) fn decode_destination(raw: &str) -> TextResult<(RelPath, Option<String>)> {
    let (path_raw, symbol_raw) = match raw.split_once('#') {
        Some((path, symbol)) => (path, Some(symbol)),
        None => (raw, None),
    };
    if symbol_raw.is_some_and(str::is_empty) {
        return Err(TextError::Destination {
            reason: "destination symbol is empty",
        });
    }
    if path_raw.contains('#') || symbol_raw.is_some_and(|symbol| symbol.contains('#')) {
        return Err(TextError::Destination {
            reason: "destination has more than one structural '#'",
        });
    }
    let path_text = decode_destination_component(path_raw)?;
    let path = RelPath::new(&path_text).map_err(|_| TextError::Destination {
        reason: "destination path is not a canonical repository-relative path",
    })?;
    let symbol = symbol_raw.map(decode_destination_component).transpose()?;
    Ok((path, symbol))
}

/// Decodes one destination component: a path, or a symbol, but not both.
pub(crate) fn decode_destination_component(raw: &str) -> TextResult<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte != b'%' {
            // A literal byte that the encoder would have escaped means this
            // text was not written by `destination`, so accepting it would let
            // two spellings decode to the same value.
            if !is_destination_literal(byte) {
                return Err(TextError::Destination {
                    reason: "destination contains a byte that must be percent-encoded",
                });
            }
            out.push(byte);
            index += 1;
            continue;
        }
        let high = bytes
            .get(index + 1)
            .copied()
            .and_then(upper_hex_value)
            .ok_or(TextError::Destination {
                reason: "destination has a truncated or non-uppercase escape",
            })?;
        let low = bytes
            .get(index + 2)
            .copied()
            .and_then(upper_hex_value)
            .ok_or(TextError::Destination {
                reason: "destination has a truncated or non-uppercase escape",
            })?;
        let decoded = (high << 4) | low;
        if is_destination_literal(decoded) {
            return Err(TextError::Destination {
                reason: "destination escapes a byte that must appear literally",
            });
        }
        out.push(decoded);
        index += 3;
    }
    String::from_utf8(out).map_err(|_| TextError::Destination {
        reason: "destination does not decode to UTF-8",
    })
}

const fn upper_hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Escapes text that is rendered as Markdown prose — a link label or a heading.
///
/// Only characters that can actually change how the line parses are escaped.
/// Escaping every ASCII punctuation mark would be simpler to write and worse to
/// read: `token\_test\.rs` in every heading of every map costs tokens and
/// legibility for names that were never ambiguous to begin with.
///
/// The rules, each guarding one real ambiguity:
///
/// - `\`, `` ` ``, `*`, `[`, `]`, `<` always start something.
/// - `_` only creates emphasis at a word boundary, so `foo_bar` is left alone
///   while `_foo_` is escaped.
/// - `&` only matters when an entity reference could follow it.
/// - `!` only matters immediately before a link.
/// - `#` only matters at the start of the text or after a space, where it could
///   be read as a heading marker or an ATX closing sequence.
///
/// This is display only. The parser recomputes the expected label from the
/// record and compares, so no inverse of this function is needed and no second
/// spelling can be smuggled through it.
pub(crate) fn escape_label(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let characters: Vec<char> = text.chars().collect();
    for (index, &character) in characters.iter().enumerate() {
        let previous = index.checked_sub(1).map(|before| characters[before]);
        let next = characters.get(index + 1).copied();
        if needs_backslash(character, previous, next) {
            out.push('\\');
        }
        out.push(character);
    }
    out
}

fn needs_backslash(character: char, previous: Option<char>, next: Option<char>) -> bool {
    match character {
        '\\' | '`' | '*' | '[' | ']' | '<' => true,
        '_' => {
            !previous.is_some_and(char::is_alphanumeric) || !next.is_some_and(char::is_alphanumeric)
        }
        '&' => next.is_some_and(|after| after.is_ascii_alphanumeric() || after == '#'),
        '!' => next == Some('['),
        '#' => previous.is_none() || previous == Some(' '),
        _ => false,
    }
}

/// Wraps text in a code span that survives whatever the text contains.
///
/// `CommonMark` gives code spans no escape character at all, so the only way to
/// include a backtick is to fence with a longer run — which is why this exists
/// instead of another arm in [`escape_label`]. A leading or trailing backtick
/// additionally needs padding spaces, because the reader strips one space from
/// each end when both are present.
///
/// Two inputs have no code-span representation at all in `CommonMark` and are
/// therefore preconditions rather than cases: text containing a line break, and
/// empty text. Both are ruled out upstream — a projection refuses a snapshot
/// with an empty name, signature, or path, and every displayed string has
/// already been folded to one line. If one ever arrives anyway the output is
/// text [`take_code_span`] refuses, which fails the staging validation loudly
/// instead of publishing a file that reads as prose.
pub(crate) fn code_span(text: &str) -> String {
    let fence = "`".repeat(longest_backtick_run(text) + 1);
    let needs_padding =
        text.starts_with('`') || text.ends_with('`') || would_lose_edge_spaces(text);
    let mut out = String::with_capacity(text.len() + 2 * fence.len() + 2);
    out.push_str(&fence);
    if needs_padding {
        out.push(' ');
    }
    out.push_str(text);
    if needs_padding {
        out.push(' ');
    }
    out.push_str(&fence);
    out
}

/// `CommonMark` strips one space from each end of a code span when both ends have
/// one and the content is not all spaces. Content matching that description
/// needs padding, or the reader eats a space the snapshot actually contained.
fn would_lose_edge_spaces(text: &str) -> bool {
    text.starts_with(' ') && text.ends_with(' ') && !text.trim().is_empty()
}

fn longest_backtick_run(text: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for character in text.chars() {
        if character == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

/// Reads one code span from the start of `line`, returning it and the rest.
///
/// The inverse of [`code_span`], and only that: it accepts the fencing this
/// crate emits and refuses anything else rather than falling back to a lenient
/// scan that would let two different spellings parse to the same record.
///
/// # Errors
/// Returns [`TextError::Record`] when the line does not begin with a closed
/// code span.
pub(crate) fn take_code_span(line: &str) -> TextResult<(String, &str)> {
    let fence_length = line.len() - line.trim_start_matches('`').len();
    if fence_length == 0 {
        return Err(TextError::Record {
            reason: "expected a code span",
        });
    }
    let fence = &line[..fence_length];
    let body_start = fence_length;
    let mut search = body_start;
    let (content, rest) = loop {
        let relative = line[search..].find(fence).ok_or(TextError::Record {
            reason: "code span is never closed",
        })?;
        let close = search + relative;
        // A longer run than the fence is content, not the close: skip past all
        // of it so `` `a``b` `` cannot be split in the middle of a run.
        let run_end = close + line[close..].len() - line[close..].trim_start_matches('`').len();
        if run_end - close == fence_length {
            break (&line[body_start..close], &line[run_end..]);
        }
        search = run_end;
    };
    let unpadded = if would_lose_edge_spaces(content) {
        &content[1..content.len() - 1]
    } else {
        content
    };
    Ok((unpadded.to_owned(), rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_destination_keeps_paths_readable_and_escapes_the_rest() {
        assert_eq!(destination("src/auth/token.rs", None), "src/auth/token.rs");
        assert_eq!(destination("src/a b.rs", None), "src/a%20b.rs");
        assert_eq!(destination("src/(x).rs", None), "src/%28x%29.rs");
        assert_eq!(destination("src/é.rs", None), "src/%C3%A9.rs");
        assert_eq!(destination("a.rs", Some("f#g")), "a.rs#f%23g");
    }

    /// The two encoders disagree on purpose, and this is the pair that proves
    /// a caller cannot use one where the other belongs.
    #[test]
    fn the_two_encoders_do_not_agree_on_the_same_path() {
        let path = "src/a b.rs";
        assert_eq!(source_anchor(path, None), "src/a b.rs");
        assert_eq!(destination(path, None), "src/a%20b.rs");
    }

    #[test]
    fn a_destination_round_trips() {
        for (path, symbol) in [
            ("src/auth/token.rs", Some("verify_token")),
            ("src/a b/c(d).rs", Some("é")),
            ("x.rs", None),
        ] {
            let encoded = destination(path, symbol);
            let (back_path, back_symbol) = decode_destination(&encoded).unwrap();
            assert_eq!(back_path.as_str(), path);
            assert_eq!(back_symbol.as_deref(), symbol);
        }
    }

    #[test]
    fn a_destination_rejects_every_non_canonical_spelling() {
        assert!(decode_destination("src/a%2fb.rs").is_err(), "lowercase hex");
        assert!(decode_destination("src/a%2F.rs").is_err(), "escaped slash");
        assert!(decode_destination("src/a%2.rs").is_err(), "truncated");
        assert!(decode_destination("src/a b.rs").is_err(), "raw space");
        assert!(decode_destination("../a.rs").is_err(), "traversal");
        assert!(decode_destination("%C3%28").is_err(), "invalid utf-8");
        assert!(decode_destination("a.rs#").is_err(), "empty symbol");
        assert!(decode_destination("a.rs#b#c").is_err(), "two separators");
    }

    #[test]
    fn a_label_escapes_only_what_would_change_the_parse() {
        assert_eq!(
            escape_label("token_test.rs#verifies"),
            "token_test.rs#verifies"
        );
        assert_eq!(escape_label("_private"), "\\_private");
        assert_eq!(escape_label("a*b"), "a\\*b");
        assert_eq!(escape_label("a[b]"), "a\\[b\\]");
        assert_eq!(escape_label("Vec<T>"), "Vec\\<T>");
        assert_eq!(escape_label("a&amp"), "a\\&amp");
        assert_eq!(escape_label("a & b"), "a & b");
        assert_eq!(escape_label("#top"), "\\#top");
        assert_eq!(escape_label("a #b"), "a \\#b");
        assert_eq!(escape_label("a#b"), "a#b");
    }

    #[test]
    fn a_code_span_survives_its_own_contents() {
        assert_eq!(code_span("fn f()"), "`fn f()`");
        assert_eq!(code_span("a`b"), "``a`b``");
        assert_eq!(code_span("a``b"), "```a``b```");
        assert_eq!(code_span("`x`"), "`` `x` ``");
        assert_eq!(code_span(" x "), "`  x  `");
        assert_eq!(code_span(" "), "` `");
    }

    #[test]
    fn a_code_span_round_trips() {
        for text in [
            "fn f()", "a`b", "a``b", "`x`", " ", "   ", " x ", "x ", "a ` `` b",
        ] {
            let rendered = code_span(text);
            let (parsed, rest) = take_code_span(&rendered).unwrap();
            assert_eq!(parsed, text, "for {text:?} rendered as {rendered:?}");
            assert_eq!(rest, "");
        }
    }

    #[test]
    fn a_code_span_stops_at_its_own_fence() {
        let (parsed, rest) = take_code_span("`a` — rest").unwrap();
        assert_eq!(parsed, "a");
        assert_eq!(rest, " — rest");
    }

    #[test]
    fn an_unclosed_code_span_is_refused() {
        assert!(take_code_span("`a").is_err());
        assert!(take_code_span("plain").is_err());
    }

    /// `CommonMark` cannot express an empty code span. The contract is that no
    /// such record exists; this pins the failure as loud rather than silent so
    /// a future caller that breaks the precondition finds out at staging.
    #[test]
    fn an_empty_code_span_is_refused_rather_than_guessed() {
        assert!(take_code_span(&code_span("")).is_err());
    }
}
