//! The strict, line-oriented reader for the bytes [`super::render`] writes.
//!
//! This parser is authoritative and deliberately narrow. It accepts exactly one
//! spelling of everything and refuses the rest, including spellings a permissive
//! YAML or Markdown library would happily normalize. That refusal is the point:
//! a parser that accepts a second spelling and rewrites it turns every
//! regeneration into a silent edit of a committed file, and turns
//! `generated_hash` into a value that means nothing.
//!
//! It also never repairs. A file that does not parse keeps its bytes and
//! becomes a reported conflict, because the only files this crate is allowed to
//! replace are the ones it can prove it wrote.

use crate::path::RelPath;

use super::digest::Digest;
use super::model::Fidelity;
use super::render;
use super::{encode, TextError, TextResult, TEXT_FORMAT_VERSION};

/// A Git conflict marker, checked before anything else.
///
/// Detected first because a conflicted file is *two* files, and every later
/// diagnostic about it would be an accident of which side happened to come
/// first in the byte stream.
const CONFLICT_MARKERS: [&str; 3] = ["<<<<<<< ", "=======", ">>>>>>> "];

/// Which page of a scope a parsed map is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageKind {
    /// The `MAP.md` router, the only page with a purpose slot.
    Router,
    /// A generated overflow page.
    Overflow { level: u32, ordinal: u32 },
}

/// One parsed map file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMap {
    scope: String,
    page: PageKind,
    fidelity: Fidelity,
    index_hash: Digest,
    api_hash: Digest,
    generated_hash: Digest,
    tokens: u64,
    budget: u64,
    purpose: Option<String>,
    children: Vec<String>,
    api: Vec<ParsedApiRecord>,
    tests: Vec<String>,
    /// Whether the stored `generated_hash` matches the file's generated region.
    owned: bool,
}

/// One `## API` entry as the file states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedApiRecord {
    /// The repository-relative file the `###` heading named.
    pub file: String,
    pub name: String,
    pub signature: String,
    /// The decoded link target, relative to the map's own directory.
    pub target: String,
}

impl ParsedMap {
    /// The `scope:` value, `.` for the repository root.
    #[must_use]
    pub fn scope(&self) -> &str {
        &self.scope
    }

    #[must_use]
    pub const fn page(&self) -> PageKind {
        self.page
    }

    #[must_use]
    pub const fn fidelity(&self) -> Fidelity {
        self.fidelity
    }

    #[must_use]
    pub const fn index_hash(&self) -> Digest {
        self.index_hash
    }

    #[must_use]
    pub const fn api_hash(&self) -> Digest {
        self.api_hash
    }

    #[must_use]
    pub const fn generated_hash(&self) -> Digest {
        self.generated_hash
    }

    #[must_use]
    pub const fn tokens(&self) -> u64 {
        self.tokens
    }

    #[must_use]
    pub const fn budget(&self) -> u64 {
        self.budget
    }

    /// The validated slot content, present only on a router.
    #[must_use]
    pub fn purpose(&self) -> Option<&str> {
        self.purpose.as_deref()
    }

    /// Whether the stored `generated_hash` describes this file's own bytes.
    ///
    /// False means somebody edited a generated region. The file is still
    /// parsed — a caller needs its scope to report *which* file — but it is
    /// never replaced.
    #[must_use]
    pub const fn is_owned(&self) -> bool {
        self.owned
    }

    #[must_use]
    pub fn children(&self) -> &[String] {
        &self.children
    }

    #[must_use]
    pub fn api(&self) -> &[ParsedApiRecord] {
        &self.api
    }

    #[must_use]
    pub fn tests(&self) -> &[String] {
        &self.tests
    }
}

/// One parsed `.rr/SYMBOLS.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSymbols {
    fidelity: Fidelity,
    index_hash: Digest,
    generated_hash: Digest,
    declared: u64,
    records: Vec<ParsedSymbolRecord>,
    owned: bool,
}

/// One `.rr/SYMBOLS.md` record, decoded.
///
/// The file stores `symbol` and `map` percent-encoded so that no field can
/// carry a tab or a newline. They are handed back decoded: a reader that had to
/// decode them itself would need an encoder this crate does not publish, and
/// two decoders is how the format acquires a second reading. `anchor` is
/// already issue #7's own spelling and is passed through untouched, because
/// that string is meant to be pasted into `rr query` verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSymbolRecord {
    pub symbol: String,
    pub visibility: String,
    pub map: String,
    pub anchor: String,
    pub line: u32,
    pub api_hash: Digest,
}

impl ParsedSymbols {
    #[must_use]
    pub const fn fidelity(&self) -> Fidelity {
        self.fidelity
    }

    #[must_use]
    pub const fn index_hash(&self) -> Digest {
        self.index_hash
    }

    #[must_use]
    pub const fn generated_hash(&self) -> Digest {
        self.generated_hash
    }

    #[must_use]
    pub const fn is_owned(&self) -> bool {
        self.owned
    }

    /// The count the file declares, which the parser has already checked
    /// against the number of records it actually holds.
    #[must_use]
    pub const fn declared_symbols(&self) -> u64 {
        self.declared
    }

    #[must_use]
    pub fn records(&self) -> &[ParsedSymbolRecord] {
        &self.records
    }
}

/// Parses one `MAP.md` or `MAP.rr-*.md`.
///
/// # Errors
/// Returns the [`TextError`] naming the first thing that is not the supported
/// grammar. Callers that need to report several problems at once parse several
/// files, not several errors from one file: a file stops being interpretable
/// after its first grammatical surprise.
pub fn parse_map(bytes: &[u8]) -> crate::Result<ParsedMap> {
    Ok(parse_map_strict(bytes)?)
}

/// Parses one `.rr/SYMBOLS.md`.
///
/// # Errors
/// As [`parse_map`].
pub fn parse_symbols(bytes: &[u8]) -> crate::Result<ParsedSymbols> {
    Ok(parse_symbols_strict(bytes)?)
}

fn parse_map_strict(bytes: &[u8]) -> TextResult<ParsedMap> {
    let text = decode(bytes)?;
    let (fields, body) = split_frontmatter(&text)?;
    expect_keys(
        &fields,
        &[
            "type",
            "format",
            "scope",
            "page",
            "fidelity",
            "index_hash",
            "api_hash",
            "generated_hash",
            "tokens",
            "budget",
        ],
    )?;
    if text_field(&fields, "type")? != render::MAP_TYPE {
        return Err(TextError::Frontmatter {
            reason: "type is not rr-map",
        });
    }
    check_format(number_field(&fields, "format")?)?;

    let scope = text_field(&fields, "scope")?.to_owned();
    let page = parse_page(text_field(&fields, "page")?)?;
    let fidelity = Fidelity::parse(text_field(&fields, "fidelity")?)?;
    let index_hash = Digest::parse(text_field(&fields, "index_hash")?)?;
    let api_hash = Digest::parse(text_field(&fields, "api_hash")?)?;
    let generated_hash = Digest::parse(text_field(&fields, "generated_hash")?)?;
    let tokens = number_field(&fields, "tokens")?;
    let budget = number_field(&fields, "budget")?;

    let mut reader = Body::new(body);
    reader.expect(render::MAP_BANNER)?;
    let title = match page {
        PageKind::Router => render::ROUTER_TITLE,
        PageKind::Overflow { .. } => render::PAGE_TITLE,
    };
    reader.expect(&format!("{title}{scope}"))?;

    let purpose = match page {
        PageKind::Router => Some(reader.read_purpose()?),
        PageKind::Overflow { .. } => None,
    };
    if purpose.is_none() && body.contains(render::SLOT_OPEN) {
        return Err(TextError::Marker {
            reason: "an overflow page carries a purpose slot",
        });
    }

    let children = reader.read_section(render::CHILDREN_HEADING, parse_link_label)?;
    let api = reader.read_api_section()?;
    let tests = reader.read_section(render::TESTS_HEADING, parse_link_label)?;
    reader.expect_blank()?;
    reader.expect(render::MERGE_FOOTER)?;
    reader.expect_end()?;

    let owned = verify_generated_hash(&fields, body, purpose.as_deref(), generated_hash);

    Ok(ParsedMap {
        scope,
        page,
        fidelity,
        index_hash,
        api_hash,
        generated_hash,
        tokens,
        budget,
        purpose,
        children,
        api,
        tests,
        owned,
    })
}

fn parse_symbols_strict(bytes: &[u8]) -> TextResult<ParsedSymbols> {
    let text = decode(bytes)?;
    let (fields, body) = split_frontmatter(&text)?;
    expect_keys(
        &fields,
        &[
            "type",
            "format",
            "fidelity",
            "index_hash",
            "generated_hash",
            "symbols",
        ],
    )?;
    if text_field(&fields, "type")? != render::SYMBOLS_TYPE {
        return Err(TextError::Frontmatter {
            reason: "type is not rr-symbols",
        });
    }
    check_format(number_field(&fields, "format")?)?;
    let fidelity = Fidelity::parse(text_field(&fields, "fidelity")?)?;
    let index_hash = Digest::parse(text_field(&fields, "index_hash")?)?;
    let generated_hash = Digest::parse(text_field(&fields, "generated_hash")?)?;
    let declared = number_field(&fields, "symbols")?;

    let mut reader = Body::new(body);
    reader.expect(render::SYMBOLS_BANNER)?;
    reader.expect(render::SYMBOLS_TITLE)?;
    reader.expect(render::SYMBOLS_COLUMNS)?;

    let mut records = Vec::new();
    while let Some(line) = reader.next_line() {
        records.push(parse_symbol_record(line)?);
    }
    if records.len() as u64 != declared {
        return Err(TextError::Record {
            reason: "the symbols count does not match the number of records",
        });
    }

    let owned = verify_generated_hash(&fields, body, None, generated_hash);
    Ok(ParsedSymbols {
        fidelity,
        index_hash,
        generated_hash,
        declared,
        records,
        owned,
    })
}

/// Decodes bytes into the one text encoding this format has.
///
/// Rejects a BOM, mixed newlines, a bare carriage return, and NUL — each of
/// which would make "the same file" a different byte string on a different
/// machine, which is the one property every comparison here depends on.
fn decode(bytes: &[u8]) -> TextResult<String> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Err(TextError::Newline {
            reason: "file begins with a byte-order mark",
        });
    }
    let text = std::str::from_utf8(bytes).map_err(|_| TextError::NotUtf8)?;
    if text.contains('\u{0}') {
        return Err(TextError::Newline {
            reason: "file contains a NUL byte",
        });
    }
    if has_conflict_markers(text) {
        return Err(TextError::MergeConflict);
    }
    normalize_newlines(text)
}

/// Whether this text holds a Git conflict marker.
///
/// Asked before ownership: a conflicted rr file starts with `<<<<<<< HEAD` and
/// so no longer looks like one of ours, and reporting it as a stranger's file
/// would send its author looking for a collision that never happened.
pub(crate) fn has_conflict_markers(text: &str) -> bool {
    CONFLICT_MARKERS
        .iter()
        .any(|marker| text.lines().any(|line| line.starts_with(marker)))
}

/// The newline contract, for callers outside this module.
///
/// `.rr/.gitignore` is not a generated artifact with frontmatter, but it is
/// still a file this crate rewrites, and rewriting it under a second newline
/// rule is how a repository ends up with two.
pub(crate) fn normalize_newlines_public(text: &str) -> TextResult<String> {
    normalize_newlines(text)
}

/// Accepts all-LF or all-CRLF, normalizes CRLF to LF, refuses anything else.
///
/// A mixed file is refused rather than normalized because the mixture is
/// evidence that two tools disagreed about the file, and picking one of them
/// silently is how the disagreement becomes permanent.
fn normalize_newlines(text: &str) -> TextResult<String> {
    let carriage_returns = text.matches('\r').count();
    if carriage_returns == 0 {
        return Ok(text.to_owned());
    }
    let pairs = text.matches("\r\n").count();
    if pairs != carriage_returns {
        return Err(TextError::Newline {
            reason: "file mixes bare carriage returns with line feeds",
        });
    }
    let line_feeds = text.matches('\n').count();
    if pairs != line_feeds {
        return Err(TextError::Newline {
            reason: "file mixes CRLF and LF line endings",
        });
    }
    Ok(text.replace("\r\n", "\n"))
}

/// One frontmatter field, in the order the file stated it.
type Fields = Vec<(String, RawValue)>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum RawValue {
    Text(String),
    Number(u64),
}

fn split_frontmatter(text: &str) -> TextResult<(Fields, &str)> {
    let rest = text.strip_prefix("---\n").ok_or(TextError::Frontmatter {
        reason: "file does not open with an exact --- delimiter line",
    })?;
    let end = rest.find("\n---\n").ok_or(TextError::Frontmatter {
        reason: "frontmatter has no closing --- delimiter line",
    })?;
    let (head, tail) = rest.split_at(end);
    let body = &tail["\n---\n".len()..];

    let mut fields = Fields::new();
    for line in head.split('\n') {
        fields.push(parse_field(line)?);
    }
    Ok((fields, body))
}

/// Parses one `key: value` line of the rr-owned YAML subset.
fn parse_field(line: &str) -> TextResult<(String, RawValue)> {
    let (key, value) = line.split_once(": ").ok_or(TextError::Frontmatter {
        reason: "frontmatter line is not exactly `key: value`",
    })?;
    if key.is_empty() || key.trim() != key {
        return Err(TextError::Frontmatter {
            reason: "frontmatter key is empty or padded with spaces",
        });
    }
    if value.starts_with('"') {
        return Ok((key.to_owned(), RawValue::Text(unquote(value)?)));
    }
    // Everything that is not a quoted string must be an unsigned base-10
    // integer. That rules out YAML's implicit booleans, nulls, floats,
    // octal-looking numbers, aliases, anchors, and tags in one test rather
    // than thirteen.
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(TextError::Frontmatter {
            reason: "frontmatter value is neither a quoted string nor an unsigned integer",
        });
    }
    if value.len() > 1 && value.starts_with('0') {
        return Err(TextError::Frontmatter {
            reason: "frontmatter integer has a leading zero",
        });
    }
    let number = value.parse().map_err(|_| TextError::Frontmatter {
        reason: "frontmatter integer does not fit",
    })?;
    Ok((key.to_owned(), RawValue::Number(number)))
}

/// Reads one double-quoted JSON string and nothing else.
fn unquote(value: &str) -> TextResult<String> {
    let inner = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .ok_or(TextError::Frontmatter {
            reason: "quoted value is not closed",
        })?;
    let mut out = String::with_capacity(inner.len());
    let mut characters = inner.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            if character == '"' {
                return Err(TextError::Frontmatter {
                    reason: "quoted value contains an unescaped quote",
                });
            }
            if (character as u32) < 0x20 {
                return Err(TextError::Frontmatter {
                    reason: "quoted value contains a raw control character",
                });
            }
            out.push(character);
            continue;
        }
        match characters.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('u') => {
                let digits: String = characters.by_ref().take(4).collect();
                let code =
                    u32::from_str_radix(&digits, 16).map_err(|_| TextError::Frontmatter {
                        reason: "quoted value has a malformed \\u escape",
                    })?;
                out.push(char::from_u32(code).ok_or(TextError::Frontmatter {
                    reason: "quoted value escapes a value that is not a character",
                })?);
            }
            _ => {
                return Err(TextError::Frontmatter {
                    reason: "quoted value has an unsupported escape",
                })
            }
        }
    }
    Ok(out)
}

/// Requires exactly these keys, once each, in this order.
fn expect_keys(fields: &Fields, expected: &[&str]) -> TextResult<()> {
    if fields.len() != expected.len() {
        return Err(TextError::Frontmatter {
            reason: "frontmatter does not have exactly the expected keys",
        });
    }
    for (field, key) in fields.iter().zip(expected) {
        if field.0 != *key {
            return Err(TextError::Frontmatter {
                reason: "frontmatter keys are missing, unknown, or out of order",
            });
        }
    }
    Ok(())
}

fn text_field<'a>(fields: &'a Fields, key: &str) -> TextResult<&'a str> {
    match fields.iter().find(|field| field.0 == key) {
        Some((_, RawValue::Text(value))) => Ok(value),
        _ => Err(TextError::Frontmatter {
            reason: "a field that must be a quoted string is not one",
        }),
    }
}

fn number_field(fields: &Fields, key: &str) -> TextResult<u64> {
    match fields.iter().find(|field| field.0 == key) {
        Some((_, RawValue::Number(value))) => Ok(*value),
        _ => Err(TextError::Frontmatter {
            reason: "a field that must be an unsigned integer is not one",
        }),
    }
}

fn check_format(found: u64) -> TextResult<()> {
    let found = u32::try_from(found).unwrap_or(u32::MAX);
    if found == TEXT_FORMAT_VERSION {
        Ok(())
    } else {
        Err(TextError::UnsupportedFormat { found })
    }
}

fn parse_page(value: &str) -> TextResult<PageKind> {
    if value == "root" {
        return Ok(PageKind::Router);
    }
    let digits = value.strip_prefix("part-").ok_or(TextError::Frontmatter {
        reason: "page is neither root nor part-<level>-<ordinal>",
    })?;
    let (level, ordinal) = digits.split_once('-').ok_or(TextError::Frontmatter {
        reason: "page part is missing its ordinal",
    })?;
    Ok(PageKind::Overflow {
        level: fixed_width_number(level)?,
        ordinal: fixed_width_number(ordinal)?,
    })
}

/// Reads exactly eight decimal digits.
///
/// Fixed width is what makes an overflow page's name predictable before it is
/// generated, so a name of any other width is not one this crate wrote.
fn fixed_width_number(digits: &str) -> TextResult<u32> {
    if digits.len() != 8 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(TextError::Frontmatter {
            reason: "page level or ordinal is not eight decimal digits",
        });
    }
    digits.parse().map_err(|_| TextError::Frontmatter {
        reason: "page level or ordinal does not fit",
    })
}

/// Recomputes `generated_hash` over what the file actually holds.
///
/// Returns whether it matches. The purpose region is replaced by the same
/// sentinel the renderer uses, so a human's edit to their own slot leaves the
/// file owned; anything else does not.
fn verify_generated_hash(
    fields: &Fields,
    body: &str,
    purpose: Option<&str>,
    stored: Digest,
) -> bool {
    // Every field, unfiltered: excluding is the hash function's job, and doing
    // it in two places is how the two ended up excluding different sets.
    let carried: Vec<(&str, render::Value)> = fields
        .iter()
        .map(|(key, value)| {
            let value = match value {
                RawValue::Text(text) => render::Value::Text(text.clone()),
                RawValue::Number(number) => render::Value::Number(*number),
            };
            (key.as_str(), value)
        })
        .collect();
    let hashed_body = match purpose {
        Some(_) => match replace_purpose(body) {
            Some(replaced) => replaced,
            None => return false,
        },
        None => body.to_owned(),
    };
    render::generated_hash_of(&carried, &hashed_body) == stored
}

/// Swaps the slot's contents for the renderer's sentinel.
///
/// Located by the markers rather than by searching for the purpose text, for
/// the same reason the renderer does not search: an empty purpose matches
/// everywhere and a repeated one matches in the wrong place.
fn replace_purpose(body: &str) -> Option<String> {
    let open = body.find(render::SLOT_OPEN)? + render::SLOT_OPEN.len();
    let close = body.find(render::SLOT_CLOSE)?;
    if close < open {
        return None;
    }
    let mut out = String::with_capacity(body.len());
    out.push_str(&body[..open]);
    out.push('\n');
    out.push_str(render::PURPOSE_SENTINEL);
    out.push('\n');
    out.push_str(&body[close..]);
    Some(out)
}

/// A cursor over body lines.
struct Body<'a> {
    lines: Vec<&'a str>,
    cursor: usize,
}

impl<'a> Body<'a> {
    fn new(body: &'a str) -> Self {
        // A body always ends with exactly one LF, so the split leaves one
        // trailing empty piece that is structure rather than content.
        let mut lines: Vec<&str> = body.split('\n').collect();
        if lines.last() == Some(&"") {
            lines.pop();
        }
        Self { lines, cursor: 0 }
    }

    fn peek(&self) -> Option<&'a str> {
        self.lines.get(self.cursor).copied()
    }

    fn next_line(&mut self) -> Option<&'a str> {
        let line = self.peek()?;
        self.cursor += 1;
        Some(line)
    }

    fn expect(&mut self, exact: &str) -> TextResult<()> {
        if self.next_line() == Some(exact) {
            Ok(())
        } else {
            Err(TextError::Marker {
                reason: "a required generated line is missing or altered",
            })
        }
    }

    fn expect_blank(&mut self) -> TextResult<()> {
        self.expect("")
    }

    fn expect_end(&mut self) -> TextResult<()> {
        if self.cursor == self.lines.len() {
            Ok(())
        } else {
            Err(TextError::Record {
                reason: "the file continues past its final generated line",
            })
        }
    }

    /// Reads the one purpose slot a router has.
    fn read_purpose(&mut self) -> TextResult<String> {
        self.expect_blank()?;
        self.expect(render::PURPOSE_HEADING)?;
        self.expect(render::SLOT_OPEN)?;
        let mut content: Vec<&str> = Vec::new();
        loop {
            let line = self.next_line().ok_or(TextError::Marker {
                reason: "purpose closing marker missing",
            })?;
            if line == render::SLOT_CLOSE {
                break;
            }
            if line == render::SLOT_OPEN {
                return Err(TextError::Marker {
                    reason: "purpose slot is opened twice",
                });
            }
            content.push(line);
        }
        if self.lines[self.cursor..].contains(&render::SLOT_OPEN) {
            return Err(TextError::Marker {
                reason: "the file has more than one purpose slot",
            });
        }
        let purpose = content.join("\n");
        super::purpose::validate(&purpose)?;
        Ok(purpose)
    }

    /// Reads one `##` section as a list of records.
    fn read_section<T>(
        &mut self,
        heading: &str,
        parse_line: fn(&str) -> TextResult<T>,
    ) -> TextResult<Vec<T>> {
        self.expect_blank()?;
        self.expect(heading)?;
        let mut records = Vec::new();
        if self.peek() == Some(render::EMPTY_SECTION) {
            self.cursor += 1;
            return Ok(records);
        }
        while let Some(line) = self.peek() {
            if line.is_empty() {
                break;
            }
            self.cursor += 1;
            records.push(parse_line(line)?);
        }
        if records.is_empty() {
            return Err(TextError::Record {
                reason: "a section is neither empty-marked nor populated",
            });
        }
        Ok(records)
    }

    /// Reads `## API`, which alone carries `###` file headings.
    fn read_api_section(&mut self) -> TextResult<Vec<ParsedApiRecord>> {
        self.expect_blank()?;
        self.expect(render::API_HEADING)?;
        let mut records = Vec::new();
        if self.peek() == Some(render::EMPTY_SECTION) {
            self.cursor += 1;
            return Ok(records);
        }
        let mut file: Option<String> = None;
        while let Some(line) = self.peek() {
            if line.is_empty() {
                break;
            }
            self.cursor += 1;
            if let Some(heading) = line.strip_prefix("### ") {
                file = Some(unescape_label(heading));
                continue;
            }
            let owner = file.clone().ok_or(TextError::Record {
                reason: "an API record appears before its file heading",
            })?;
            records.push(parse_api_line(&owner, line)?);
        }
        if records.is_empty() {
            return Err(TextError::Record {
                reason: "a section is neither empty-marked nor populated",
            });
        }
        Ok(records)
    }
}

/// Reads `- [label](<destination>)` and returns the label.
fn parse_link_label(line: &str) -> TextResult<String> {
    let (label, _) = split_link(line)?;
    Ok(label)
}

/// Reads one `## API` record.
///
/// ```text
/// - `name` — `signature` — [source](<destination>)
/// ```
fn parse_api_line(file: &str, line: &str) -> TextResult<ParsedApiRecord> {
    let rest = line.strip_prefix("- ").ok_or(TextError::Record {
        reason: "an API record does not begin with a list marker",
    })?;
    let (name, rest) = encode::take_code_span(rest)?;
    let rest = rest.strip_prefix(" — ").ok_or(TextError::Record {
        reason: "an API record is missing the separator after its name",
    })?;
    let (signature, rest) = encode::take_code_span(rest)?;
    let rest = rest.strip_prefix(" — ").ok_or(TextError::Record {
        reason: "an API record is missing the separator after its signature",
    })?;
    let (label, destination) = split_link(rest)?;
    if label != "source" {
        return Err(TextError::Record {
            reason: "an API record's link is not labelled source",
        });
    }
    let (path, symbol) = encode::decode_destination(&destination)?;
    // A record displays a possibly-qualified name and anchors the bare one, so
    // the two are not required to be equal — only to describe one symbol. That
    // is the single relationship between them, and a record whose link points
    // somewhere else is a record no reader should trust.
    if symbol.as_deref() != Some(trailing_identifier(&name)) {
        return Err(TextError::Record {
            reason: "an API record links to a symbol other than the one it names",
        });
    }
    Ok(ParsedApiRecord {
        file: file.to_owned(),
        name,
        signature,
        target: path.as_str().to_owned(),
    })
}

/// The last identifier in a possibly-qualified name.
///
/// Any non-identifier character is treated as a separator, so this holds for
/// `a::b::c`, `A.b`, and a bare `c` alike rather than for one language's
/// spelling of qualification.
fn trailing_identifier(name: &str) -> &str {
    match name.rfind(|character: char| !is_identifier_char(character)) {
        Some(index) => {
            let separator = name[index..].chars().next().map_or(0, char::len_utf8);
            &name[index + separator..]
        }
        None => name,
    }
}

fn is_identifier_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// Splits `- [label](<destination>)` or `[label](<destination>)`.
fn split_link(line: &str) -> TextResult<(String, String)> {
    let rest = line.strip_prefix("- ").unwrap_or(line);
    let rest = rest.strip_prefix('[').ok_or(TextError::Record {
        reason: "a link record does not begin with a label",
    })?;
    let close = rest.rfind("](<").ok_or(TextError::Record {
        reason: "a link record has no destination",
    })?;
    let label = &rest[..close];
    let destination = rest[close + "](<".len()..]
        .strip_suffix(">)")
        .ok_or(TextError::Record {
            reason: "a link destination is not closed",
        })?;
    Ok((unescape_label(label), destination.to_owned()))
}

/// Removes the backslashes [`encode::escape_label`] added.
///
/// A plain removal: every backslash in a generated label precedes the character
/// it escapes, and a label that held a real backslash had it escaped too.
fn unescape_label(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut escaped = false;
    for character in label.chars() {
        if escaped {
            out.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            out.push(character);
        }
    }
    out
}

fn parse_symbol_record(line: &str) -> TextResult<ParsedSymbolRecord> {
    let fields: Vec<&str> = line.split('\t').collect();
    let [symbol, visibility, map, anchor, line_number, hash] = fields[..] else {
        return Err(TextError::Record {
            reason: "a symbol record does not have exactly six tab-separated fields",
        });
    };
    let visibility = visibility.to_owned();
    let anchor = anchor.to_owned();
    let api_hash = Digest::parse(hash)?;
    if !matches!(visibility.as_str(), "public" | "crate" | "restricted") {
        return Err(TextError::Record {
            reason: "a symbol record's visibility is not public, crate, or restricted",
        });
    }
    let line = line_number.parse::<u32>().map_err(|_| TextError::Record {
        reason: "a symbol record's line is not a base-10 number",
    })?;
    if line == 0 {
        return Err(TextError::Record {
            reason: "a symbol record's line is not positive",
        });
    }
    // Decoded rather than merely inspected: a destination that does not decode
    // is a record pointing at a file nobody can open.
    let (map, _) = encode::decode_destination(map)?;
    let map = map.as_str().to_owned();
    // A symbol is one component, not a destination: decoding it as a path would
    // hand `RelPath` a name it was never asked to judge.
    let symbol = encode::decode_destination_component(symbol)?;
    Ok(ParsedSymbolRecord {
        symbol,
        visibility,
        map,
        anchor,
        line,
        api_hash,
    })
}

/// Whether a path names a file this module owns the right to write.
///
/// Name-based, and deliberately generous about overflow pages: any file
/// matching the reserved pattern is rr's namespace whether or not the current
/// plan uses that exact ordinal. A repository that puts its own file there has
/// taken a name the generator reserved.
#[must_use]
pub fn is_reserved_map_path(path: &RelPath) -> bool {
    let Some(name) = path.file_name() else {
        return false;
    };
    name == super::MAP_FILE_NAME
        || (name.starts_with(super::OVERFLOW_PREFIX) && name.ends_with(super::MARKDOWN_EXTENSION))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_is_normalized_and_mixtures_are_refused() {
        assert_eq!(normalize_newlines("a\r\nb\r\n").unwrap(), "a\nb\n");
        assert_eq!(normalize_newlines("a\nb\n").unwrap(), "a\nb\n");
        assert!(normalize_newlines("a\r\nb\n").is_err(), "mixed");
        assert!(normalize_newlines("a\rb").is_err(), "bare CR");
    }

    #[test]
    fn a_bom_or_nul_is_refused() {
        assert!(decode("\u{feff}---\n".as_bytes()).is_err());
        assert!(decode(b"---\nnul\0\n").is_err());
        assert!(decode(&[0xFF, 0xFE]).is_err());
    }

    #[test]
    fn git_conflict_markers_are_found_before_anything_else() {
        let text = "---\n<<<<<<< ours\ntype: \"rr-map\"\n=======\n>>>>>>> theirs\n";
        assert!(matches!(
            decode(text.as_bytes()),
            Err(TextError::MergeConflict)
        ));
    }

    #[test]
    fn only_the_supported_scalar_spellings_parse() {
        assert!(parse_field("format: 1").is_ok());
        assert!(parse_field("scope: \"src\"").is_ok());
        assert!(parse_field("format: 01").is_err(), "leading zero");
        assert!(parse_field("format: yes").is_err(), "implicit boolean");
        assert!(parse_field("format: ~").is_err(), "implicit null");
        assert!(parse_field("scope: src").is_err(), "bare string");
        assert!(parse_field("scope: 'src'").is_err(), "single quotes");
        assert!(parse_field("scope: !!str src").is_err(), "tag");
        assert!(parse_field("scope: &a src").is_err(), "anchor");
        assert!(parse_field("scope:  \"src\"").is_err(), "extra space");
        assert!(parse_field("# comment").is_err(), "comment");
    }

    #[test]
    fn keys_must_be_exactly_these_in_exactly_this_order() {
        let ordered = vec![
            ("a".to_owned(), RawValue::Number(1)),
            ("b".to_owned(), RawValue::Number(2)),
        ];
        assert!(expect_keys(&ordered, &["a", "b"]).is_ok());
        assert!(expect_keys(&ordered, &["b", "a"]).is_err());
        assert!(expect_keys(&ordered, &["a"]).is_err());
        assert!(expect_keys(&ordered, &["a", "b", "c"]).is_err());
    }

    #[test]
    fn a_label_round_trips_through_its_escaping() {
        for label in ["plain", "_private", "a*b", "Vec<T>", "a\\b", "a[b]"] {
            assert_eq!(unescape_label(&encode::escape_label(label)), label);
        }
    }

    #[test]
    fn a_page_label_is_fixed_width() {
        assert_eq!(parse_page("root").unwrap(), PageKind::Router);
        assert_eq!(
            parse_page("part-00000001-00000002").unwrap(),
            PageKind::Overflow {
                level: 1,
                ordinal: 2
            }
        );
        assert!(parse_page("part-1-2").is_err());
        assert!(parse_page("part-00000001").is_err());
        assert!(parse_page("other").is_err());
    }
}
