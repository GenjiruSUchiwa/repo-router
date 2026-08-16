use std::fmt::Write as _;

use serde::Serialize;

use crate::index::Snapshot;
use crate::path::RelPath;
use crate::ranking::RankingEvidence;
use crate::result::{resolve_anchor, Confidence, NoneReason, Pipeline, QueryResult};
use crate::verify::{SourcePacket, SourceResult, SourceStatus};
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
    render_answer_text(snapshot, result)
}

/// Renders the text output contract preceded by one `explain:` line.
///
/// The diagnostic goes **before** the answer so the answer keeps the shape a
/// caller already knows: without `--source` the anchor is still the last line
/// of the output, and with `--source` it is still the first line of the answer,
/// followed by the source markers and the bounded content.
///
/// `evidence` is `None` when the query routed exactly, which reads no posting
/// list and so has nothing to explain about the candidate cap.
///
/// # Errors
/// Returns an error if anchor resolution fails.
pub fn render_text_explained(
    snapshot: &Snapshot,
    result: &QueryResult,
    evidence: Option<&RankingEvidence>,
) -> Result<String> {
    let mut out = match evidence {
        None => "explain: exact route, nothing ranked\n".to_owned(),
        Some(evidence) => format!(
            "explain: postings={} union={} scored={} dropped={} arbitrary_cut={} terms={}\n",
            evidence.posting_hits_scanned,
            evidence.candidates_before_cap,
            evidence.candidates_scored,
            evidence.candidates_dropped,
            if evidence.cap_cut_a_tie { "yes" } else { "no" },
            evidence.effective_query_terms,
        ),
    };
    out.push_str(&render_answer_text(snapshot, result)?);
    Ok(out)
}

fn render_answer_text(snapshot: &Snapshot, result: &QueryResult) -> Result<String> {
    result.validate()?;
    match result {
        QueryResult::Direct {
            candidate, source, ..
        } => {
            let anchor = resolve_anchor(snapshot, candidate.target)?;
            let encoded = encode_anchor(anchor.path, anchor.symbol);
            let mut out = format!("FINAL SOURCE ANCHOR (copy exactly): {encoded}\n");
            if let Some(source) = source {
                write_source_text(&mut out, anchor.path, source)?;
            }
            Ok(out)
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

/// Appends the source packet or refusal to the text answer.
///
/// Every marker but one is printed straight from a packet field, so text and
/// JSON cannot disagree about what was served. The exception is `SOURCE BYTES`,
/// which the renderer computes and computes *here* rather than in the packet,
/// because it describes this encoding rather than the packet: it counts the
/// bytes between the `---` fence and the structural LF, and only the text
/// contract has a fence to bound. JSON needs no equivalent — its content is one
/// string member and cannot escape its own field, which is why `--source` was
/// never forgeable there.
///
/// The content is followed by one structural LF that is not part of the source.
/// That LF is what `SOURCE FINAL NEWLINE` distinguishes the content's own last
/// byte from, and it is deliberately outside `SOURCE BYTES`: counting it would
/// let a file that does not end in a newline be described by two markers of the
/// same output that contradict each other.
fn write_source_text(out: &mut String, path: &str, source: &SourceResult) -> Result<()> {
    let path = encode_anchor(path, None);
    match source {
        SourceResult::Served(packet) => {
            write_served_source_text(out, &path, packet);
            Ok(())
        }
        SourceResult::Refused {
            status: SourceStatus::Stale,
        } => {
            let _ = writeln!(
                out,
                "STALE SOURCE (no content returned): {path} changed since indexing; run `rr refresh`"
            );
            Ok(())
        }
        SourceResult::Refused {
            status: SourceStatus::Verified,
        } => Err(Error::SnapshotInvariant {
            reason: "a refused source cannot carry the verified status",
        }),
        SourceResult::Refused { status } => {
            let _ = writeln!(
                out,
                "SOURCE REFUSED ({}; no content returned): {path} {}",
                status.as_str(),
                refusal_detail(*status)
            );
            Ok(())
        }
    }
}

fn write_served_source_text(out: &mut String, path: &str, packet: &SourcePacket) {
    let span = packet.span();
    let (window_start, window_end) = packet.served_lines();
    let _ = writeln!(
        out,
        "SOURCE SPAN (verified): {path}:{}-{}",
        span.start_line(),
        span.end_line()
    );
    let _ = writeln!(out, "SOURCE WINDOW: {path}:{window_start}-{window_end}");
    let _ = writeln!(
        out,
        "SOURCE REPRESENTATION: {}",
        packet.representation().as_str()
    );
    if packet.complete() {
        out.push_str("SOURCE COMPLETE\n");
    } else {
        let _ = writeln!(
            out,
            "SOURCE TRUNCATED ({} anchor lines, {} anchor bytes omitted)",
            packet.omitted_anchor_lines(),
            packet.omitted_anchor_bytes()
        );
    }
    if packet.context_clipped() {
        out.push_str("SOURCE CONTEXT CLIPPED\n");
    }
    out.push_str(if packet.ends_with_newline() {
        "SOURCE FINAL NEWLINE: present\n"
    } else {
        "SOURCE FINAL NEWLINE: absent\n"
    });
    // Bound once, used twice, in that order. Reading the content into a local
    // before the count is written is what makes it impossible for a later edit
    // to change what is served without changing what the count claims — the two
    // lines below cannot be made to disagree without deleting this one.
    let content = packet.content();
    let _ = writeln!(out, "SOURCE BYTES: {}", content.len());
    out.push_str("---\n");
    out.push_str(content);
    out.push('\n');
}

/// The stable explanation attached to each expected refusal.
///
/// `rr refresh` is suggested only where refreshing the index is what actually
/// resolves the refusal; a size or line-length refusal survives a refresh.
const fn refusal_detail(status: SourceStatus) -> &'static str {
    match status {
        SourceStatus::Missing => "no longer exists; run `rr refresh`",
        SourceStatus::Symlink | SourceStatus::NotRegular => {
            "is not a regular source file; run `rr refresh`"
        }
        SourceStatus::TooLarge => "is larger than the 16 MiB verification limit",
        SourceStatus::LineTooLong => "has a line larger than the 64 KiB source limit",
        SourceStatus::NotText => "is not UTF-8 text; nothing was decoded",
        SourceStatus::Raced => "changed during verification; retry or run `rr refresh`",
        SourceStatus::Stale | SourceStatus::Verified => "",
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
        #[serde(skip_serializing_if = "Option::is_none")]
        source: Option<&'a SourceResult>,
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

/// The `explain` member of an explained JSON response.
///
/// Serializes as `null` for an exact route, which reads no posting list. The
/// member is absent entirely unless the caller asked to be told, so the v1
/// response is unchanged byte for byte by default.
#[derive(Debug, Serialize, PartialEq)]
#[serde(transparent)]
struct JsonExplain(Option<RankingEvidence>);

/// A v1 response carrying the optional diagnostic member.
///
/// The response is flattened rather than duplicated into each variant, so the
/// answer contract has exactly one definition and the diagnostic cannot drift
/// between the three shapes it can accompany.
#[derive(Debug, Serialize, PartialEq)]
struct JsonEnvelope<'a> {
    #[serde(flatten)]
    response: JsonResponse<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    explain: Option<JsonExplain>,
}

/// Renders a [`QueryResult`] into the single-line JSON v1 contract.
///
/// # Errors
/// Returns an error if anchor resolution or serialization fails.
pub fn render_json(snapshot: &Snapshot, result: &QueryResult) -> Result<String> {
    render_json_envelope(snapshot, result, None)
}

/// Renders the JSON v1 contract with an added `explain` member.
///
/// `evidence` is `None` when the query routed exactly, and the member is then
/// `null`: the route is explained by having read nothing.
///
/// # Errors
/// Returns an error if anchor resolution or serialization fails.
pub fn render_json_explained(
    snapshot: &Snapshot,
    result: &QueryResult,
    evidence: Option<&RankingEvidence>,
) -> Result<String> {
    render_json_envelope(snapshot, result, Some(JsonExplain(evidence.copied())))
}

fn render_json_envelope<'a>(
    snapshot: &'a Snapshot,
    result: &'a QueryResult,
    explain: Option<JsonExplain>,
) -> Result<String> {
    result.validate()?;
    let dto = match result {
        QueryResult::Direct {
            candidate,
            pipeline,
            source,
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
                source: source.as_ref(),
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
    let envelope = JsonEnvelope {
        response: dto,
        explain,
    };
    let mut serialized =
        serde_json::to_string(&envelope).map_err(|_| Error::SnapshotInvariant {
            reason: "failed to serialize query result to JSON",
        })?;
    serialized.push('\n');
    Ok(serialized)
}
