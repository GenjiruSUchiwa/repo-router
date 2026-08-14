use std::fmt::Write as _;

use serde::Serialize;

use crate::index::Snapshot;
use crate::path::RelPath;
use crate::result::{resolve_anchor, Confidence, NoneReason, Pipeline, QueryResult};
use crate::{Error, Result};

#[must_use]
pub fn encode_anchor(path: impl AsRef<str>, symbol: Option<&str>) -> String {
    let mut out = String::new();
    encode_percent(path.as_ref(), &mut out);
    if let Some(symbol_name) = symbol {
        out.push('#');
        encode_percent(symbol_name, &mut out);
    }
    out
}

/// Decodes a machine-safe percent-encoded anchor string into its path and symbol components.
///
/// # Errors
/// Returns [`Error::SnapshotInvariant`] on malformed or non-canonical escapes.
pub fn decode_anchor(raw: &str) -> Result<(RelPath, Option<String>)> {
    if let Some((path_raw, symbol_raw)) = raw.split_once('#') {
        if symbol_raw.is_empty() {
            return Err(Error::SnapshotInvariant {
                reason: "anchor symbol cannot be empty",
            });
        }
        let path_decoded = decode_percent(path_raw)?;
        let symbol_decoded = decode_percent(symbol_raw)?;
        let path = RelPath::new(&path_decoded)?;
        Ok((path, Some(symbol_decoded)))
    } else {
        let path_decoded = decode_percent(raw)?;
        let path = RelPath::new(&path_decoded)?;
        Ok((path, None))
    }
}

fn encode_percent(input: &str, out: &mut String) {
    const HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    for character in input.chars() {
        if character == '%' || character == '#' || character.is_ascii_control() {
            let byte = character as u8;
            out.push('%');
            out.push(HEX_DIGITS[(byte >> 4) as usize] as char);
            out.push(HEX_DIGITS[(byte & 0x0F) as usize] as char);
        } else {
            out.push(character);
        }
    }
}

fn decode_percent(input: &str) -> Result<String> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(Error::SnapshotInvariant {
                    reason: "malformed percent escape in anchor",
                });
            }
            let val1 = from_hex_digit(bytes[i + 1]).ok_or(Error::SnapshotInvariant {
                reason: "invalid hex digit in percent escape",
            })?;
            let val2 = from_hex_digit(bytes[i + 2]).ok_or(Error::SnapshotInvariant {
                reason: "invalid hex digit in percent escape",
            })?;
            let byte = (val1 << 4) | val2;
            if byte != b'%' && byte != b'#' && !byte.is_ascii_control() {
                return Err(Error::SnapshotInvariant {
                    reason: "non-canonical percent escape in anchor",
                });
            }
            decoded.push(byte);
            i += 3;
        } else if bytes[i] == b'#' || bytes[i].is_ascii_control() {
            return Err(Error::SnapshotInvariant {
                reason: "unescaped reserved byte in anchor component",
            });
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| Error::SnapshotInvariant {
        reason: "invalid UTF-8 in decoded anchor",
    })
}

fn from_hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Renders a [`QueryResult`] into the human-readable text output contract.
///
/// # Errors
/// Returns an error if anchor resolution fails.
pub fn render_text(snapshot: &Snapshot, result: &QueryResult) -> Result<String> {
    result.validate()?;
    match result {
        QueryResult::Direct { candidate, .. } => {
            let anchor = resolve_anchor(snapshot, candidate.target)?;
            let encoded = encode_anchor(anchor.path, anchor.symbol);
            Ok(format!("FINAL SOURCE ANCHOR (copy exactly): {encoded}\n"))
        }
        QueryResult::Candidates { candidates, .. } => {
            let mut out = String::from("source candidates:\n");
            for (index, candidate) in candidates.iter().enumerate() {
                let anchor = resolve_anchor(snapshot, candidate.target)?;
                let encoded = encode_anchor(anchor.path, anchor.symbol);
                let position = index + 1;
                let _ = writeln!(out, "{position}. {encoded}");
            }
            Ok(out)
        }
        QueryResult::None { reason, .. } => match reason {
            NoneReason::NotFound => Ok("NO ANCHOR (index has no match); try: rr map\n".to_string()),
            NoneReason::LowConfidence => {
                Ok("NO ANCHOR (confidence too low); refine the query or use --path\n".to_string())
            }
        },
    }
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "result", rename_all = "snake_case")]
enum JsonResponse<'a> {
    Direct {
        v: u32,
        pipeline: Pipeline,
        anchor: JsonAnchor<'a>,
        confidence: f32,
    },
    Candidates {
        v: u32,
        pipeline: Pipeline,
        candidates: Vec<JsonCandidateItem<'a>>,
    },
    None {
        v: u32,
        pipeline: Pipeline,
        reason: NoneReason,
    },
}

#[derive(Debug, Serialize, PartialEq)]
pub struct JsonAnchor<'a> {
    pub path: &'a str,
    pub symbol: Option<&'a str>,
    pub lines: Option<[u32; 2]>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct JsonCandidateItem<'a> {
    pub anchor: JsonAnchor<'a>,
    pub confidence: Option<f32>,
}

/// Renders a [`QueryResult`] into the single-line JSON v1 contract.
///
/// # Errors
/// Returns an error if anchor resolution or serialization fails.
pub fn render_json(snapshot: &Snapshot, result: &QueryResult) -> Result<String> {
    result.validate()?;
    let dto = match result {
        QueryResult::Direct {
            candidate,
            pipeline,
        } => {
            let anchor = resolve_anchor(snapshot, candidate.target)?;
            let lines = anchor.lines.map(|l| [l.start(), l.end()]);
            let confidence = candidate
                .confidence
                .ok_or(Error::SnapshotInvariant {
                    reason: "direct result is missing confidence",
                })?
                .get();
            JsonResponse::Direct {
                v: 1,
                pipeline: *pipeline,
                anchor: JsonAnchor {
                    path: anchor.path,
                    symbol: anchor.symbol,
                    lines,
                },
                confidence,
            }
        }
        QueryResult::Candidates {
            candidates,
            pipeline,
        } => {
            let mut items = Vec::with_capacity(candidates.len());
            for candidate in candidates {
                let anchor = resolve_anchor(snapshot, candidate.target)?;
                let lines = anchor.lines.map(|l| [l.start(), l.end()]);
                let confidence = candidate.confidence.map(Confidence::get);
                items.push(JsonCandidateItem {
                    anchor: JsonAnchor {
                        path: anchor.path,
                        symbol: anchor.symbol,
                        lines,
                    },
                    confidence,
                });
            }
            JsonResponse::Candidates {
                v: 1,
                pipeline: *pipeline,
                candidates: items,
            }
        }
        QueryResult::None { reason, pipeline } => JsonResponse::None {
            v: 1,
            pipeline: *pipeline,
            reason: *reason,
        },
    };
    let mut serialized = serde_json::to_string(&dto).map_err(|_| Error::SnapshotInvariant {
        reason: "failed to serialize query result to JSON",
    })?;
    serialized.push('\n');
    Ok(serialized)
}
