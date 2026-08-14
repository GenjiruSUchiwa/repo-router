use std::borrow::Cow;

use unicode_normalization::{is_nfc_quick, IsNormalized, UnicodeNormalization};

use crate::Result;

/// Tokenizes an input string into canonical lexemes using the shared boundary rules.
///
/// # Errors
/// Propagates any error returned by the callback `f`.
pub fn for_each_lexeme<F>(input: &str, f: F) -> Result<()>
where
    F: FnMut(&str) -> Result<()>,
{
    let stripped = strip_raw_markers(input);
    let stripped = stripped.as_ref();
    if stripped.is_empty() {
        return Ok(());
    }

    if stripped.is_ascii() {
        for_each_ascii_lexeme(stripped, f)
    } else {
        for_each_unicode_lexeme(stripped, f)
    }
}
#[must_use]
pub fn is_canonical_term(term: &str) -> bool {
    if term.is_empty() {
        return false;
    }

    if !term.chars().all(char::is_alphanumeric) {
        return false;
    }

    let nfc_reconstructed: String = term.chars().nfc().collect();
    if nfc_reconstructed != term {
        return false;
    }

    let lower_nfc: String = term.chars().flat_map(char::to_lowercase).nfc().collect();
    lower_nfc == term
}

/// Strips Rust raw-identifier markers (`r#`) at the start of the input and after
/// any non-alphanumeric boundary, so `crate::auth::r#type` and a bare `r#type`
/// canonicalize identically. Borrows when no embedded marker is present.
fn strip_raw_markers(input: &str) -> Cow<'_, str> {
    let leading = input.strip_prefix("r#").unwrap_or(input);

    let mut out: Option<String> = None;
    let mut copied_to = 0usize;
    let mut search_from = 0usize;
    while let Some(rel) = leading[search_from..].find("r#") {
        let pos = search_from + rel;
        let at_boundary = !leading[..pos]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric);
        if at_boundary {
            let buf = out.get_or_insert_with(|| String::with_capacity(leading.len()));
            buf.push_str(&leading[copied_to..pos]);
            copied_to = pos + 2;
        }
        search_from = pos + 2;
    }

    match out {
        Some(mut buf) => {
            buf.push_str(&leading[copied_to..]);
            Cow::Owned(buf)
        }
        None => Cow::Borrowed(leading),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Separator,
    Digit,
    Upper,
    Lower,
    UncasedAlpha,
}

const fn classify_byte(b: u8) -> CharClass {
    if b.is_ascii_digit() {
        CharClass::Digit
    } else if b.is_ascii_uppercase() {
        CharClass::Upper
    } else if b.is_ascii_lowercase() {
        CharClass::Lower
    } else {
        CharClass::Separator
    }
}

fn classify_char(c: char) -> CharClass {
    if !c.is_alphanumeric() {
        CharClass::Separator
    } else if c.is_numeric() {
        CharClass::Digit
    } else if c.is_uppercase() {
        CharClass::Upper
    } else if c.is_lowercase() {
        CharClass::Lower
    } else {
        CharClass::UncasedAlpha
    }
}

/// Single implementation of the token-boundary state machine, shared by the
/// ASCII and Unicode paths via a `CharClass` accessor.
fn scan_segments<C, E>(len: usize, class_at: C, mut emit: E) -> Result<()>
where
    C: Fn(usize) -> CharClass,
    E: FnMut(usize, usize) -> Result<()>,
{
    let mut i = 0usize;

    while i < len {
        while i < len && class_at(i) == CharClass::Separator {
            i += 1;
        }
        if i >= len {
            break;
        }

        let start = i;
        if class_at(i) == CharClass::Digit {
            while i < len && class_at(i) == CharClass::Digit {
                i += 1;
            }
            emit(start, i)?;
            continue;
        }

        if class_at(i) == CharClass::UncasedAlpha {
            while i < len && class_at(i) == CharClass::UncasedAlpha {
                i += 1;
            }
            emit(start, i)?;
            continue;
        }

        while i < len {
            match class_at(i) {
                CharClass::Separator | CharClass::UncasedAlpha => break,
                CharClass::Digit => {
                    let digit_start = i;
                    while i < len && class_at(i) == CharClass::Digit {
                        i += 1;
                    }
                    if i < len && class_at(i) != CharClass::Separator {
                        break;
                    }
                    i = digit_start;
                    break;
                }
                CharClass::Upper => {
                    if i > start && class_at(i - 1) == CharClass::Lower {
                        break;
                    }
                    if i > start
                        && class_at(i - 1) == CharClass::Upper
                        && i + 1 < len
                        && class_at(i + 1) == CharClass::Lower
                    {
                        break;
                    }
                    i += 1;
                }
                CharClass::Lower => i += 1,
            }
        }

        if i > start {
            emit(start, i)?;
        }
    }

    Ok(())
}

fn for_each_ascii_lexeme<F>(input: &str, mut f: F) -> Result<()>
where
    F: FnMut(&str) -> Result<()>,
{
    let bytes = input.as_bytes();
    scan_segments(
        bytes.len(),
        |i| classify_byte(bytes[i]),
        |start, end| emit_ascii_segment(&input[start..end], &mut f),
    )
}

fn emit_ascii_segment<F>(segment: &str, f: &mut F) -> Result<()>
where
    F: FnMut(&str) -> Result<()>,
{
    if segment.bytes().all(|b| !b.is_ascii_uppercase()) {
        f(segment)
    } else {
        let mut stack_buf = [0u8; 64];
        if segment.len() <= stack_buf.len() {
            for (idx, b) in segment.bytes().enumerate() {
                stack_buf[idx] = b.to_ascii_lowercase();
            }
            let lower =
                std::str::from_utf8(&stack_buf[..segment.len()]).expect("valid ascii conversion");
            f(lower)
        } else {
            let mut heap_buf = Vec::with_capacity(segment.len());
            for b in segment.bytes() {
                heap_buf.push(b.to_ascii_lowercase());
            }
            let lower = std::str::from_utf8(&heap_buf).expect("valid ascii conversion");
            f(lower)
        }
    }
}

fn for_each_unicode_lexeme<F>(input: &str, mut f: F) -> Result<()>
where
    F: FnMut(&str) -> Result<()>,
{
    let nfc_input: Cow<'_, str> = match is_nfc_quick(input.chars()) {
        IsNormalized::Yes => Cow::Borrowed(input),
        IsNormalized::No | IsNormalized::Maybe => Cow::Owned(input.chars().nfc().collect()),
    };
    let text = nfc_input.as_ref();
    let marks: Vec<(usize, CharClass)> = text
        .char_indices()
        .map(|(offset, c)| (offset, classify_char(c)))
        .collect();

    scan_segments(
        marks.len(),
        |i| marks[i].1,
        |start, end| {
            let start_byte = marks[start].0;
            let end_byte = marks.get(end).map_or(text.len(), |m| m.0);
            emit_unicode_segment(&text[start_byte..end_byte], &mut f)
        },
    )
}

fn lowercases_to_self(c: char) -> bool {
    let mut lower = c.to_lowercase();
    lower.next() == Some(c) && lower.next().is_none()
}

fn emit_unicode_segment<F>(segment: &str, f: &mut F) -> Result<()>
where
    F: FnMut(&str) -> Result<()>,
{
    let already_canonical = segment.chars().all(lowercases_to_self)
        && matches!(is_nfc_quick(segment.chars()), IsNormalized::Yes);
    if already_canonical {
        return f(segment);
    }

    let mut term: String = segment.chars().flat_map(char::to_lowercase).nfc().collect();
    for _ in 0..4 {
        let stable: String = term
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .nfc()
            .collect();
        if stable == term {
            break;
        }
        term = stable;
    }
    if term.is_empty() || !is_canonical_term(&term) {
        return Ok(());
    }
    f(&term)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const GOLDEN_CASES: [(&str, &[&str]); 34] = [
        ("", &[]),
        ("___", &[]),
        ("verify", &["verify"]),
        ("Verify", &["verify"]),
        ("verify_token", &["verify", "token"]),
        ("verify-token", &["verify", "token"]),
        ("verifyToken", &["verify", "token"]),
        ("VerifyToken", &["verify", "token"]),
        ("XMLHttpRequest", &["xml", "http", "request"]),
        ("XMLHttpRequest2", &["xml", "http", "request", "2"]),
        ("utf8Decode", &["utf8", "decode"]),
        ("sha256Digest", &["sha256", "digest"]),
        ("3DModel", &["3", "d", "model"]),
        ("foo42", &["foo", "42"]),
        ("foo42Bar", &["foo42", "bar"]),
        ("foo::bar", &["foo", "bar"]),
        ("AuthService.validate", &["auth", "service", "validate"]),
        ("src/auth/token.rs", &["src", "auth", "token", "rs"]),
        ("src\\auth\\token.rs", &["src", "auth", "token", "rs"]),
        ("r#type", &["type"]),
        ("r#async_fn", &["async", "fn"]),
        ("r#", &[]),
        ("crate::auth::r#type", &["crate", "auth", "type"]),
        ("foo::r#async::bar", &["foo", "async", "bar"]),
        ("JWTValidator", &["jwt", "validator"]),
        ("héllo_wörld", &["héllo", "wörld"]),
        ("E\u{0301}clair", &["éclair"]),
        ("Éclair", &["éclair"]),
        ("МоскваHTTPClient", &["москва", "http", "client"]),
        ("東京駅", &["東京駅"]),
        ("東京_HTTP", &["東京", "http"]),
        ("foo🙂bar", &["foo", "bar"]),
        ("ΣParser", &["σ", "parser"]),
        ("İd", &["id"]),
    ];

    #[test]
    fn test_all_golden_cases() {
        for (input, expected) in GOLDEN_CASES {
            let mut terms = Vec::new();
            for_each_lexeme(input, |term| {
                terms.push(term.to_string());
                Ok(())
            })
            .unwrap();

            assert_eq!(
                terms.as_slice(),
                expected,
                "golden case failed for input: {input:?}"
            );
        }
    }

    #[test]
    fn test_canonical_term_validation() {
        assert!(is_canonical_term("verify"));
        assert!(is_canonical_term("token"));
        assert!(is_canonical_term("éclair"));
        assert!(is_canonical_term("東京駅"));
        assert!(is_canonical_term("42"));
        assert!(is_canonical_term("sha256"));
        assert!(is_canonical_term("sha"));
        assert!(is_canonical_term("256"));

        assert!(!is_canonical_term(""));
        assert!(!is_canonical_term("Verify"));
        assert!(!is_canonical_term("verify_token"));
        assert!(!is_canonical_term("verify token"));
        assert!(!is_canonical_term("E\u{0301}clair"));
        assert!(!is_canonical_term("foo🙂bar"));
        assert!(!is_canonical_term("i\u{0307}d"));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(500))]

        #[test]
        fn prop_ascii_and_unicode_paths_agree_on_ascii(input in "[ -~]{0,48}") {
            let mut via_ascii = Vec::new();
            for_each_ascii_lexeme(&input, |t| {
                via_ascii.push(t.to_string());
                Ok(())
            })
            .unwrap();

            let mut via_unicode = Vec::new();
            for_each_unicode_lexeme(&input, |t| {
                via_unicode.push(t.to_string());
                Ok(())
            })
            .unwrap();

            prop_assert_eq!(via_ascii, via_unicode);
        }
    }
}
