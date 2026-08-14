use unicode_normalization::UnicodeNormalization;

use crate::Result;

/// Tokenizes an input string into canonical lexemes using the shared boundary rules.
///
/// # Errors
/// Propagates any error returned by the callback `f`.
pub fn for_each_lexeme<F>(input: &str, f: F) -> Result<()>
where
    F: FnMut(&str) -> Result<()>,
{
    let stripped = strip_raw_prefix(input);
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

fn strip_raw_prefix(input: &str) -> &str {
    if let Some(suffix) = input.strip_prefix("r#") {
        if suffix.is_empty() {
            ""
        } else {
            suffix
        }
    } else {
        input
    }
}

fn for_each_ascii_lexeme<F>(input: &str, mut f: F) -> Result<()>
where
    F: FnMut(&str) -> Result<()>,
{
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;

    while i < len {
        while i < len && !bytes[i].is_ascii_alphanumeric() {
            i += 1;
        }
        if i >= len {
            break;
        }

        let start = i;
        if bytes[i].is_ascii_digit() {
            while i < len && bytes[i].is_ascii_digit() {
                i += 1;
            }
            emit_ascii_segment(&input[start..i], &mut f)?;
            continue;
        }

        while i < len {
            if !bytes[i].is_ascii_alphanumeric() {
                break;
            }

            if bytes[i].is_ascii_digit() {
                let digit_start = i;
                while i < len && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i < len && bytes[i].is_ascii_alphabetic() {
                    break;
                }
                i = digit_start;
                break;
            }

            if bytes[i].is_ascii_uppercase() {
                if i > start && bytes[i - 1].is_ascii_lowercase() {
                    break;
                }
                if i > start
                    && bytes[i - 1].is_ascii_uppercase()
                    && i + 1 < len
                    && bytes[i + 1].is_ascii_lowercase()
                {
                    break;
                }
            }

            i += 1;
        }

        let segment = &input[start..i];
        if !segment.is_empty() {
            emit_ascii_segment(segment, &mut f)?;
        }
    }

    Ok(())
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Separator,
    Digit,
    Upper,
    Lower,
    UncasedAlpha,
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

fn for_each_unicode_lexeme<F>(input: &str, mut f: F) -> Result<()>
where
    F: FnMut(&str) -> Result<()>,
{
    let nfc_input: String = input.chars().nfc().collect();
    let chars: Vec<char> = nfc_input.chars().collect();
    let classes: Vec<CharClass> = chars.iter().copied().map(classify_char).collect();
    let len = chars.len();
    let mut i = 0usize;

    while i < len {
        while i < len && classes[i] == CharClass::Separator {
            i += 1;
        }
        if i >= len {
            break;
        }

        let start = i;
        if classes[i] == CharClass::Digit {
            while i < len && classes[i] == CharClass::Digit {
                i += 1;
            }
            emit_unicode_segment(&chars[start..i], &mut f)?;
            continue;
        }

        if classes[i] == CharClass::UncasedAlpha {
            while i < len && classes[i] == CharClass::UncasedAlpha {
                i += 1;
            }
            emit_unicode_segment(&chars[start..i], &mut f)?;
            continue;
        }

        while i < len {
            if classes[i] == CharClass::Separator || classes[i] == CharClass::UncasedAlpha {
                break;
            }

            if classes[i] == CharClass::Digit {
                let digit_start = i;
                while i < len && classes[i] == CharClass::Digit {
                    i += 1;
                }
                if i < len
                    && (classes[i] == CharClass::Upper
                        || classes[i] == CharClass::Lower
                        || classes[i] == CharClass::UncasedAlpha)
                {
                    break;
                }
                i = digit_start;
                break;
            }

            if classes[i] == CharClass::Upper {
                if i > start && classes[i - 1] == CharClass::Lower {
                    break;
                }
                if i > start
                    && classes[i - 1] == CharClass::Upper
                    && i + 1 < len
                    && classes[i + 1] == CharClass::Lower
                {
                    break;
                }
            }

            i += 1;
        }

        let slice = &chars[start..i];
        if !slice.is_empty() {
            emit_unicode_segment(slice, &mut f)?;
        }
    }

    Ok(())
}

fn emit_unicode_segment<F>(chars: &[char], f: &mut F) -> Result<()>
where
    F: FnMut(&str) -> Result<()>,
{
    let lower_nfc: String = chars.iter().flat_map(|c| c.to_lowercase()).nfc().collect();
    f(&lower_nfc)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN_CASES: [(&str, &[&str]); 32] = [
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
        ("JWTValidator", &["jwt", "validator"]),
        ("héllo_wörld", &["héllo", "wörld"]),
        ("E\u{0301}clair", &["éclair"]),
        ("Éclair", &["éclair"]),
        ("МоскваHTTPClient", &["москва", "http", "client"]),
        ("東京駅", &["東京駅"]),
        ("東京_HTTP", &["東京", "http"]),
        ("foo🙂bar", &["foo", "bar"]),
        ("ΣParser", &["σ", "parser"]),
        ("İd", &["i̇d"]),
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
    }
}
