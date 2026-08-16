//! Generic facts extraction from a grammar's `queries/tags.scm`.

use std::ops::Range;
use std::sync::OnceLock;

use tree_sitter_language::LanguageFn;
use tree_sitter_tags::{TagsConfiguration, TagsContext};

use crate::facts::{
    def_key, reference_key, Def, DefKind, DegradedReason, Facts, OwnerIndex, ParseStatus,
    Reference, ReferenceKind, Span, TestSignals, Visibility,
};
use crate::lang::Lang;
use crate::{Error, Result};

use super::{degraded_facts, scan_idents, Extractor};

const MAX_SIGNATURE_BYTES: usize = 180;

/// Static configuration for one grammar-backed tags extractor.
///
/// The query and grammar are compiled into their crates. Keeping the complete
/// specification static lets the registry retain a `const` table of bare
/// builder function pointers.
pub struct LanguageSpec {
    pub lang: Lang,
    pub language: LanguageFn,
    pub tags_query: &'static str,
    pub locals_query: &'static str,
    /// Definition capture suffixes, without the `definition.` prefix.
    pub kinds: &'static [(&'static str, DefKind)],
    /// Reference capture suffixes, without the `reference.` prefix.
    pub reference_kinds: &'static [(&'static str, ReferenceKind)],
    /// Visibility judged from the bare definition name — the only evidence a
    /// tags query surfaces.
    pub visibility: fn(&str) -> Visibility,
    /// Whether one attribute identifier marks the definition as a test.
    pub test_attribute: fn(&str) -> bool,
    /// Whether an enclosing definition of this name is a test scope.
    pub test_scope: fn(&str) -> bool,
    /// Last word on one definition, once its name, kind, signature and
    /// visibility are all in hand.
    ///
    /// `tree-sitter-tags` compiles text predicates and then never evaluates
    /// them, so `#eq?` and `#match?` cannot route a capture. Anything a
    /// language decides by reading the name or the declaration text — a
    /// `constructor` that is not an ordinary method, an accessor that is not
    /// either, an `= () =>` binding that is a function wearing a variable's
    /// syntax, a `private` modifier no capture can reach — is decided here
    /// instead.
    ///
    /// Runs before the definitions are sorted, because [`def_key`] holds the
    /// kind and a kind changed afterwards would leave the order it was sorted
    /// into.
    pub refine: fn(&mut Def),
    /// Whether this language's documentation is the string literal that opens
    /// a body, as Python's docstring is.
    ///
    /// `false` for a language whose documentation is a comment *preceding* the
    /// definition: the comment already falls outside the span, so nothing has
    /// to be kept out of the body scan — and excluding a leading body string
    /// anyway would silently drop a `"use strict"` prologue's identifiers from
    /// a documented function.
    pub doc_is_leading_body_string: bool,
    /// The compiled tags query, shared by every worker that speaks this
    /// language. Compiling it costs milliseconds that rayon would otherwise
    /// repeat once per work split; only [`TagsContext`] stays per worker.
    pub config: OnceLock<std::result::Result<TagsConfiguration, String>>,
}

/// Stateful generic tags extractor: one tags parser/context per worker.
pub struct TagsExtractor {
    spec: &'static LanguageSpec,
    config: &'static TagsConfiguration,
    context: TagsContext,
}

impl TagsExtractor {
    /// Compiles and validates one language's tags query, once per process.
    ///
    /// # Errors
    /// Returns a construction error when the query is invalid or contains a
    /// definition/reference capture with no kind mapping.
    pub fn new(spec: &'static LanguageSpec) -> std::result::Result<Self, String> {
        let config = spec.config.get_or_init(|| {
            let language: tree_sitter::Language = spec.language.into();
            let config = TagsConfiguration::new(language, spec.tags_query, spec.locals_query)
                .map_err(|error| error.to_string())?;
            validate_kind_maps(spec, &config)?;
            Ok(config)
        });
        match config {
            Ok(config) => Ok(Self {
                spec,
                config,
                context: TagsContext::new(),
            }),
            Err(message) => Err(message.clone()),
        }
    }

    /// Extracts validated facts from exactly these bytes.
    ///
    /// Encoding, parser, and tags-iteration failures become lexical degraded
    /// facts. Structural invariant failures remain errors so they cannot be
    /// hidden as a plausible but incomplete API.
    ///
    /// # Errors
    /// Returns extraction invariant or facts validation failures.
    pub fn extract(&mut self, content: &[u8]) -> Result<Facts> {
        if content.len() > u32::MAX as usize {
            return Ok(degraded_facts(content, DegradedReason::SourceTooLarge));
        }
        let Ok(source) = std::str::from_utf8(content) else {
            return Ok(degraded_facts(content, DegradedReason::InvalidUtf8));
        };
        let lines = LineIndex::new(content)?;
        let spec = self.spec;
        let config = self.config;
        let Ok((tags, parse_errors)) = self.context.generate_tags(config, content, None) else {
            return Ok(degraded_facts(content, DegradedReason::ParserReturnedNone));
        };

        let mut defs: Vec<(Def, Vec<Span>)> = Vec::new();
        let mut references = Vec::new();
        for item in tags {
            let Ok(tag) = item else {
                return Ok(degraded_facts(content, DegradedReason::ParserReturnedNone));
            };
            if tag.name_range.start >= tag.name_range.end {
                continue;
            }
            let Some(name) = source.get(tag.name_range.clone()) else {
                continue;
            };
            if tag.is_definition {
                let Some(kind) = definition_kind(spec, config, tag.syntax_type_id) else {
                    continue;
                };
                let span = span_for_range(&tag.range, &lines, source)?;
                let header = header_for(
                    span,
                    tag.name_range.start,
                    name,
                    tag.docs.is_some() && spec.doc_is_leading_body_string,
                    source,
                    &lines,
                )?;
                let doc_idents = tag.docs.as_deref().map(scan_idents).unwrap_or_default();
                let test_signals = TestSignals {
                    explicit_attribute: header
                        .attribute_idents
                        .iter()
                        .any(|ident| (spec.test_attribute)(ident)),
                    inside_cfg_test: false,
                    inside_test_scope: false,
                };
                let mut def = Def {
                    name: name.to_owned(),
                    local_qualified: None,
                    kind,
                    visibility: (spec.visibility)(name),
                    span,
                    signature_span: header.signature_span,
                    signature: header.signature,
                    signature_idents: header.signature_idents,
                    body_idents: Vec::new(),
                    doc_idents,
                    attribute_idents: header.attribute_idents,
                    test_signals,
                };
                (spec.refine)(&mut def);
                defs.push((def, header.exclusions));
            } else {
                let Some(kind) = reference_kind(spec, config, tag.syntax_type_id) else {
                    continue;
                };
                references.push(Reference {
                    name: name.to_owned(),
                    qualified: None,
                    kind,
                    span: span_for_range(&tag.name_range, &lines, source)?,
                    owner: None,
                });
            }
        }

        defs.sort_by(|left, right| def_key(&left.0).cmp(&def_key(&right.0)));
        defs.dedup_by(|left, right| def_key(&left.0) == def_key(&right.0));
        let (mut defs, header_exclusions): (Vec<Def>, Vec<Vec<Span>>) = defs.into_iter().unzip();
        assign_nesting(&mut defs, &header_exclusions, source, spec);

        references.sort_by(|left, right| reference_key(left).cmp(&reference_key(right)));
        let owners = OwnerIndex::new(&defs);
        for reference in &mut references {
            reference.owner = owners.nearest(reference.span);
        }

        Facts::from_parts(
            defs,
            references,
            Vec::new(),
            ParseStatus::Tags { parse_errors },
        )
    }
}

impl Extractor for TagsExtractor {
    fn lang(&self) -> Lang {
        self.spec.lang
    }

    fn extract(&mut self, content: &[u8]) -> Result<Facts> {
        TagsExtractor::extract(self, content)
    }
}

fn validate_kind_maps(
    spec: &LanguageSpec,
    config: &TagsConfiguration,
) -> std::result::Result<(), String> {
    for capture in config.query.capture_names() {
        if let Some(kind) = capture.strip_prefix("definition.") {
            if !spec.kinds.iter().any(|(candidate, _)| *candidate == kind) {
                return Err(format!("missing definition kind mapping for `{kind}`"));
            }
        } else if let Some(kind) = capture.strip_prefix("reference.") {
            if !spec
                .reference_kinds
                .iter()
                .any(|(candidate, _)| *candidate == kind)
            {
                return Err(format!("missing reference kind mapping for `{kind}`"));
            }
        }
    }
    Ok(())
}

fn definition_kind(
    spec: &LanguageSpec,
    config: &TagsConfiguration,
    syntax_type_id: u32,
) -> Option<DefKind> {
    let name = config.syntax_type_name(syntax_type_id);
    spec.kinds
        .iter()
        .find_map(|(candidate, kind)| (*candidate == name).then_some(*kind))
}

fn reference_kind(
    spec: &LanguageSpec,
    config: &TagsConfiguration,
    syntax_type_id: u32,
) -> Option<ReferenceKind> {
    let name = config.syntax_type_name(syntax_type_id);
    spec.reference_kinds
        .iter()
        .find_map(|(candidate, kind)| (*candidate == name).then_some(*kind))
}

/// Byte offsets of every line start, including the empty line after a trailing
/// newline. Offsets are u32 because [`Span`] is u32-based.
struct LineIndex {
    starts: Vec<u32>,
}

impl LineIndex {
    fn new(bytes: &[u8]) -> Result<Self> {
        let mut starts = vec![0];
        for (index, &byte) in bytes.iter().enumerate() {
            if byte == b'\n' {
                let next = index.checked_add(1).ok_or(Error::ExtractionInvariant {
                    message: "line offset overflow",
                })?;
                starts.push(u32::try_from(next).map_err(|_| Error::ExtractionInvariant {
                    message: "line offset exceeds u32::MAX",
                })?);
            }
        }
        Ok(Self { starts })
    }

    fn line_for(&self, offset: usize) -> Result<u32> {
        let offset = u32::try_from(offset).map_err(|_| Error::ExtractionInvariant {
            message: "line lookup offset exceeds u32::MAX",
        })?;
        let line = self.starts.partition_point(|start| *start <= offset);
        u32::try_from(line).map_err(|_| Error::ExtractionInvariant {
            message: "line number exceeds u32::MAX",
        })
    }

    /// The byte offset at which the line containing `offset` starts.
    fn line_start(&self, offset: usize) -> usize {
        let offset = u32::try_from(offset).unwrap_or(u32::MAX);
        // `partition_point >= 1` because `starts[0]` is `0`.
        let line = self.starts.partition_point(|start| *start <= offset);
        self.starts[line - 1] as usize
    }
}

fn span_for_range(range: &Range<usize>, lines: &LineIndex, source: &str) -> Result<Span> {
    if range.start > range.end || range.end > source.len() {
        return Err(Error::ExtractionInvariant {
            message: "tags range exceeds source length",
        });
    }
    if !source.is_char_boundary(range.start) || !source.is_char_boundary(range.end) {
        return Err(Error::ExtractionInvariant {
            message: "tags range is not on a UTF-8 boundary",
        });
    }
    let start_line = lines.line_for(range.start)?;
    let end_line = if range.start == range.end {
        start_line
    } else {
        lines.line_for(range.end - 1)?
    };
    Span::new(
        u32::try_from(range.start).map_err(|_| Error::ExtractionInvariant {
            message: "tags start offset exceeds u32::MAX",
        })?,
        u32::try_from(range.end).map_err(|_| Error::ExtractionInvariant {
            message: "tags end offset exceeds u32::MAX",
        })?,
        start_line,
        end_line,
    )
}

/// The header of one tagged definition: its signature, its attached
/// attributes, and the regions the body scan must skip.
struct Header {
    signature_span: Span,
    signature: String,
    signature_idents: Vec<String>,
    attribute_idents: Vec<String>,
    /// Regions the body scan must not read: attached attributes and the
    /// captured documentation string. The signature span is excluded by the
    /// caller, which already holds it.
    exclusions: Vec<Span>,
}

fn header_for(
    span: Span,
    name_start: usize,
    name: &str,
    has_docs: bool,
    source: &str,
    lines: &LineIndex,
) -> Result<Header> {
    let span_start = span.start_byte() as usize;
    let span_end = span.end_byte() as usize;
    // The line that names the definition opens the header. Everything the span
    // holds before that line is attached attributes — a decorated Python
    // definition's span starts at its first decorator.
    let line_start = lines.line_start(name_start).max(span_start);
    let item_start = source
        .get(line_start..span_end)
        .and_then(|text| text.find(|character: char| !character.is_ascii_whitespace()))
        .map_or(line_start, |offset| line_start + offset)
        .max(span_start);

    let mut exclusions = Vec::new();
    let attribute_idents = if line_start > span_start {
        exclusions.push(span_for_range(&(span_start..line_start), lines, source)?);
        source
            .get(span_start..line_start)
            .map(scan_idents)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let sig_end = signature_end(source, item_start, span_end);
    let signature_span = span_for_range(&(item_start..sig_end), lines, source)?;
    let raw = source.get(item_start..sig_end).unwrap_or_default();
    let signature = {
        let displayed = crate::facts::display_signature(raw);
        if displayed.is_empty() {
            name.to_owned()
        } else {
            displayed
        }
    };
    // Compared against the name as the scanner sees it: a sigil is not part of
    // an identifier, so a TypeScript `#private` name would otherwise never
    // match itself and every such definition would carry its own name.
    let scanned_name = name.trim_start_matches('#');
    let signature_idents = scan_idents(raw)
        .into_iter()
        .filter(|ident| ident != scanned_name)
        .collect();

    if has_docs {
        if let Some(range) = docstring_range(source, sig_end, span_end) {
            exclusions.push(span_for_range(&range, lines, source)?);
        }
    }

    Ok(Header {
        signature_span,
        signature,
        signature_idents,
        attribute_idents,
        exclusions,
    })
}

/// Where the declaration header ends: the first line break outside brackets
/// and strings, so a parameter list wrapped across lines stays one signature.
///
/// When no such break exists before the cap — an unbalanced, malformed header —
/// this falls back to the first line so the header cannot swallow the body.
fn signature_end(source: &str, start: usize, span_end: usize) -> usize {
    let cap = span_end.min(start.saturating_add(MAX_SIGNATURE_BYTES));
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut first_newline = None;
    let mut index = start;
    while index < cap {
        let byte = bytes[index];
        if let Some(open) = quote {
            match byte {
                b'\\' => index += 1,
                // A header never wraps inside a string; treat it as malformed.
                b'\n' => {
                    first_newline.get_or_insert(index);
                    break;
                }
                _ if byte == open => quote = None,
                _ => {}
            }
        } else {
            match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                b'\n' => {
                    first_newline.get_or_insert(index);
                    if depth == 0 {
                        return index;
                    }
                }
                _ => {}
            }
        }
        index += 1;
    }
    let mut end = first_newline.unwrap_or(cap);
    while end > start && !source.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// The byte range of the string literal that opens the body after `from` —
/// the literal the tags query captured as documentation, which the body scan
/// must therefore not read as body identifiers.
///
/// `None` leaves the body scan untouched, for spans so malformed that the text
/// after the header is not a string literal after all.
fn docstring_range(source: &str, from: usize, span_end: usize) -> Option<Range<usize>> {
    let bytes = source.as_bytes();
    let mut index = from;
    while index < span_end && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    let start = index;
    // String prefixes such as `r`, `b`, `f`, `u`, alone or paired.
    while index < span_end && bytes[index].is_ascii_alphabetic() {
        index += 1;
    }
    if index >= span_end || index - start > 2 {
        return None;
    }
    let open = bytes[index];
    if open != b'"' && open != b'\'' {
        return None;
    }
    let triple = bytes
        .get(index..index + 3)
        .is_some_and(|quotes| quotes[1] == open && quotes[2] == open);
    if triple {
        let needle = if open == b'"' { "\"\"\"" } else { "'''" };
        let body = source.get(index + 3..span_end)?;
        let close = body.find(needle)?;
        return Some(start..index + 3 + close + 3);
    }
    let mut cursor = index + 1;
    while cursor < span_end {
        match bytes[cursor] {
            b'\\' => cursor += 1,
            b'\n' => return None,
            byte if byte == open => return Some(start..cursor + 1),
            _ => {}
        }
        cursor += 1;
    }
    None
}

fn assign_nesting(
    defs: &mut [Def],
    header_exclusions: &[Vec<Span>],
    source: &str,
    spec: &LanguageSpec,
) {
    let separator = spec.lang.qualified_separator();
    let mut order: Vec<usize> = (0..defs.len()).collect();
    order.sort_by(|left, right| {
        let left_span = defs[*left].span;
        let right_span = defs[*right].span;
        left_span
            .start_byte()
            .cmp(&right_span.start_byte())
            .then_with(|| right_span.end_byte().cmp(&left_span.end_byte()))
            .then_with(|| def_key(&defs[*left]).cmp(&def_key(&defs[*right])))
    });

    let mut direct_children: Vec<Vec<Span>> = vec![Vec::new(); defs.len()];
    let mut stack: Vec<usize> = Vec::new();
    for index in order {
        while stack
            .last()
            .is_some_and(|parent| !strictly_contains(defs[*parent].span, defs[index].span))
        {
            stack.pop();
        }
        if let Some(&parent) = stack.last() {
            let mut segments = stack
                .iter()
                .map(|parent| defs[*parent].name.as_str())
                .collect::<Vec<_>>();
            segments.push(defs[index].name.as_str());
            defs[index].local_qualified = Some(segments.join(separator));
            direct_children[parent].push(defs[index].span);
        }
        let inside_test_scope = stack
            .iter()
            .any(|ancestor| (spec.test_scope)(&defs[*ancestor].name));
        if inside_test_scope {
            defs[index].test_signals.inside_test_scope = true;
        }
        stack.push(index);
    }

    for (index, def) in defs.iter_mut().enumerate() {
        let mut exclusions =
            Vec::with_capacity(1 + header_exclusions[index].len() + direct_children[index].len());
        exclusions.push(def.signature_span);
        exclusions.extend(header_exclusions[index].iter().copied());
        exclusions.extend(direct_children[index].iter().copied());
        def.body_idents = scan_excluding(source, def.span, &exclusions);
    }
}

fn strictly_contains(parent: Span, child: Span) -> bool {
    parent.start_byte() <= child.start_byte()
        && child.end_byte() <= parent.end_byte()
        && (parent.start_byte() < child.start_byte() || child.end_byte() < parent.end_byte())
}

fn scan_excluding(source: &str, container: Span, excluded: &[Span]) -> Vec<String> {
    let mut ranges = excluded.to_vec();
    ranges.sort_by_key(|span| (span.start_byte(), span.end_byte()));
    let container_start = container.start_byte() as usize;
    let container_end = container.end_byte() as usize;
    let mut cursor = container_start;
    let mut idents = Vec::new();
    for span in ranges {
        let start = (span.start_byte() as usize).max(container_start);
        let end = (span.end_byte() as usize).min(container_end);
        if end <= cursor {
            continue;
        }
        if start > cursor {
            if let Some(text) = source.get(cursor..start) {
                idents.extend(scan_idents(text));
            }
        }
        cursor = end;
    }
    if cursor < container_end {
        if let Some(text) = source.get(cursor..container_end) {
            idents.extend(scan_idents(text));
        }
    }
    idents
}

/// PEP 8 as [`Visibility`] documents it: a `__mangled` name is private, a
/// `_internal` name is a weak internal-use indicator, and a `__dunder__` name
/// is the public protocol a class exposes.
fn python_visibility(name: &str) -> Visibility {
    let is_dunder = name.len() > 4 && name.starts_with("__") && name.ends_with("__");
    if is_dunder {
        Visibility::Public
    } else if name.starts_with("__") {
        Visibility::Private
    } else if name.starts_with('_') {
        Visibility::Internal
    } else {
        Visibility::Public
    }
}

/// A decorator mentioning either standard test framework marks a test, the
/// tags-tier reading of what `#[test]` states in Rust.
fn python_test_attribute(ident: &str) -> bool {
    ident == "pytest" || ident == "unittest"
}

/// pytest collects the methods of `Test*`-named classes.
fn python_test_scope(name: &str) -> bool {
    name.starts_with("Test")
}

/// The definition a language's query already described exactly.
///
/// Python needs no second pass: `class`, `def` and a module-level assignment
/// are three node types, so the query alone tells them apart, and PEP 8
/// visibility is readable from the name. `@property` stays a decorated
/// function rather than becoming a [`DefKind::Property`] — the decorator is a
/// call whose meaning depends on what it resolves to, and the tags tier does
/// not resolve.
fn keep_as_captured(_def: &mut Def) {}

pub(crate) static PYTHON: LanguageSpec = LanguageSpec {
    lang: Lang::Python,
    language: tree_sitter_python::LANGUAGE,
    tags_query: include_str!("queries/python.scm"),
    locals_query: "",
    kinds: &[
        ("constant", DefKind::Variable),
        ("class", DefKind::Class),
        ("function", DefKind::Function),
    ],
    reference_kinds: &[("call", ReferenceKind::Call)],
    visibility: python_visibility,
    test_attribute: python_test_attribute,
    test_scope: python_test_scope,
    refine: keep_as_captured,
    doc_is_leading_body_string: true,
    config: OnceLock::new(),
};

/// Every word TypeScript allows between the start of a member declaration and
/// its name.
///
/// Read as a prefix rather than searched for: a parameter called `privateKey`
/// and a field called `private` are both things a repository contains, and
/// neither declares a visibility.
const TYPESCRIPT_MODIFIERS: &[&str] = &[
    "public",
    "private",
    "protected",
    "static",
    "readonly",
    "abstract",
    "override",
    "accessor",
    "declare",
    "async",
    "get",
    "set",
];

/// The modifiers this declaration opens with, in source order.
///
/// [`crate::facts::display_signature`] has already folded every whitespace run
/// into one space, so splitting on it recovers the tokens exactly.
fn typescript_modifiers(signature: &str) -> impl Iterator<Item = &str> {
    signature
        .split(' ')
        .take_while(|word| TYPESCRIPT_MODIFIERS.contains(word))
}

/// The visibility a member states, or `None` when it states none.
fn declared_visibility(signature: &str) -> Option<Visibility> {
    typescript_modifiers(signature).find_map(|word| match word {
        "private" => Some(Visibility::Private),
        "protected" => Some(Visibility::Protected),
        "public" => Some(Visibility::Public),
        _ => None,
    })
}

/// A `#name` is private to its class by the language, not by convention: the
/// `#` is part of the name, and reading it outside the class is a syntax
/// error. Everything else is public until a modifier says otherwise, which is
/// [`declared_visibility`]'s answer rather than this one's.
fn typescript_visibility(name: &str) -> Visibility {
    if name.starts_with('#') {
        Visibility::Private
    } else {
        Visibility::Public
    }
}

/// The initializer this binding assigns, or `None` when it assigns nothing.
///
/// The `=` that opens it is the first one that is not part of an operator:
/// `const f: (x: number) => void = …` annotates before it assigns, and
/// splitting on the `=` of that `=>` would read the *type* as the initializer.
fn initializer_of(signature: &str) -> Option<&str> {
    let bytes = signature.as_bytes();
    let assignment = (0..bytes.len()).find(|index| {
        if bytes[*index] != b'=' {
            return false;
        }
        let previous = index.checked_sub(1).map(|before| bytes[before]);
        let next = bytes.get(index + 1).copied();
        // `=>`, `==`, and the tail of `!=`, `<=`, `>=`, `===`.
        !matches!(next, Some(b'=' | b'>')) && !matches!(previous, Some(b'=' | b'!' | b'<' | b'>'))
    })?;
    signature.get(assignment + 1..)
}

/// The text after a leading type-parameter list, or `None` when the `<` does
/// not close before the string ends.
///
/// `<` is the one bracket that is not reliably a bracket: it opens a type
/// parameter list, a JSX element and a comparison alike. Matching it by depth
/// is what separates `<T extends Map<string, number>,>(a: T) => a`, whose list
/// closes onto the parameters, from `<Badge onClick={() => go()} />`, which is
/// a value that happens to contain an arrow.
fn after_type_parameters(initializer: &str) -> Option<&str> {
    let mut depth = 0usize;
    for (offset, character) in initializer.char_indices() {
        match character {
            '<' => depth += 1,
            '>' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return initializer.get(offset + 1..);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether this binding's initializer is a function rather than a value.
///
/// Judged from the initializer's first token and not from `=>` appearing
/// anywhere, because `{ onEvent: () => … }` is an object and `(a) >= (b)` is a
/// comparison. A binding whose initializer starts on the next line is not
/// judged at all: the signature ends at the first line break outside brackets,
/// so there is nothing there to read, and it stays a [`DefKind::Variable`].
fn initializer_is_a_function(signature: &str) -> bool {
    let Some(initializer) = initializer_of(signature) else {
        return false;
    };
    let initializer = initializer.trim_start();
    let initializer = initializer
        .strip_prefix("async ")
        .map_or(initializer, str::trim_start);
    if initializer.starts_with("function") {
        return true;
    }
    let opens_parameters = if initializer.starts_with('<') {
        after_type_parameters(initializer).is_some_and(|rest| rest.trim_start().starts_with('('))
    } else {
        initializer.starts_with('(')
    };
    opens_parameters && initializer.contains("=>")
}

/// What the TypeScript query captured, plus what it structurally could not.
///
/// A `constructor`, a `get`/`set` accessor and an arrow-function binding are
/// all `method_definition`s or `variable_declarator`s to the grammar; only
/// their text tells them apart, and a tags query cannot read text. Nor can it
/// reach an `accessibility_modifier`, which is an unnamed sibling of the name.
fn typescript_refine(def: &mut Def) {
    if let Some(visibility) = declared_visibility(&def.signature) {
        def.visibility = visibility;
    }
    match def.kind {
        DefKind::Method => {
            if def.name == "constructor" {
                def.kind = DefKind::Constructor;
            } else if typescript_modifiers(&def.signature)
                .any(|word| word == "get" || word == "set")
            {
                def.kind = DefKind::Property;
            }
        }
        DefKind::Variable if initializer_is_a_function(&def.signature) => {
            def.kind = DefKind::Function;
        }
        _ => {}
    }
}

/// TypeScript declares no test intent on the definition.
///
/// `describe`, `it` and `test` are calls: not attributes, and not the names of
/// enclosing definitions. Reading them as a test scope here would mean
/// inventing a claim the tags tier never saw. What TypeScript does state is in
/// the file name, and [`Lang::path_indicates_test`] already reads
/// `.test.ts`/`.spec.ts`.
fn never_a_test_signal(_name: &str) -> bool {
    false
}

const TYPESCRIPT_KINDS: &[(&str, DefKind)] = &[
    ("function", DefKind::Function),
    ("class", DefKind::Class),
    ("interface", DefKind::Interface),
    ("enum", DefKind::Enum),
    ("type", DefKind::TypeAlias),
    ("namespace", DefKind::Namespace),
    ("method", DefKind::Method),
    ("field", DefKind::Field),
    ("variable", DefKind::Variable),
];

const TYPESCRIPT_REFERENCE_KINDS: &[(&str, ReferenceKind)] = &[
    ("call", ReferenceKind::Call),
    ("method", ReferenceKind::MethodCall),
];

/// One TypeScript specification, differing from the other only in the grammar
/// it compiles against.
///
/// Spelled once so the two cannot drift: everything below the first two fields
/// is the language, and the language is the same one.
const fn typescript_spec(lang: Lang, language: LanguageFn) -> LanguageSpec {
    LanguageSpec {
        lang,
        language,
        tags_query: include_str!("queries/typescript.scm"),
        locals_query: "",
        kinds: TYPESCRIPT_KINDS,
        reference_kinds: TYPESCRIPT_REFERENCE_KINDS,
        visibility: typescript_visibility,
        test_attribute: never_a_test_signal,
        test_scope: never_a_test_signal,
        refine: typescript_refine,
        doc_is_leading_body_string: false,
        config: OnceLock::new(),
    }
}

pub(crate) static TYPESCRIPT: LanguageSpec = typescript_spec(
    Lang::TypeScript,
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
);

/// TSX: the same query against a different grammar.
///
/// Tree-sitter ships two parsers because the languages disagree — `<T>(x)` is a
/// type assertion in one and an unclosed element in the other — so a `.tsx`
/// file parsed by the TypeScript grammar is a file full of syntax errors, not
/// a file that happens to also parse. The separate [`OnceLock`] is what keeps
/// them apart: a compiled `TagsConfiguration` holds its grammar, so one shared
/// between these two specs would hand every `.tsx` file the `.ts` parser —
/// which is why each of these is its own `static` rather than one built twice.
pub(crate) static TSX: LanguageSpec =
    typescript_spec(Lang::Tsx, tree_sitter_typescript::LANGUAGE_TSX);

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn line_index_matches_newline_and_eof_boundaries() {
        let source = "one\ntwo\n";
        let lines = LineIndex::new(source.as_bytes()).unwrap();
        assert_eq!(lines.line_for(0).unwrap(), 1);
        assert_eq!(lines.line_for(3).unwrap(), 1);
        assert_eq!(lines.line_for(4).unwrap(), 2);
        assert_eq!(lines.line_for(source.len()).unwrap(), 3);
        assert_eq!(lines.line_start(0), 0);
        assert_eq!(lines.line_start(3), 0);
        assert_eq!(lines.line_start(5), 4);
        assert_eq!(
            span_for_range(&(0..4), &lines, source).unwrap().end_line(),
            1
        );
        assert_eq!(
            span_for_range(&(0..5), &lines, source).unwrap().end_line(),
            2
        );
        assert_eq!(
            span_for_range(&(0..source.len()), &lines, source)
                .unwrap()
                .end_line(),
            2
        );

        let empty = LineIndex::new(b"").unwrap();
        let span = span_for_range(&(0..0), &empty, "").unwrap();
        assert_eq!((span.start_line(), span.end_line()), (1, 1));
    }

    #[test]
    fn a_grammar_without_a_usable_tags_query_falls_back_to_lexical() {
        static ILLEGAL: LanguageSpec = LanguageSpec {
            lang: Lang::Python,
            language: tree_sitter_python::LANGUAGE,
            tags_query: "(module) @illegal",
            locals_query: "",
            kinds: &[],
            reference_kinds: &[],
            visibility: python_visibility,
            test_attribute: python_test_attribute,
            test_scope: python_test_scope,
            refine: keep_as_captured,
            doc_is_leading_body_string: true,
            config: OnceLock::new(),
        };
        static INCOMPLETE: LanguageSpec = LanguageSpec {
            lang: Lang::Python,
            language: tree_sitter_python::LANGUAGE,
            tags_query: "(module) @definition.module",
            locals_query: "",
            kinds: &[],
            reference_kinds: &[],
            visibility: python_visibility,
            test_attribute: python_test_attribute,
            test_scope: python_test_scope,
            refine: keep_as_captured,
            doc_is_leading_body_string: true,
            config: OnceLock::new(),
        };
        assert!(TagsExtractor::new(&ILLEGAL).is_err());
        assert!(TagsExtractor::new(&INCOMPLETE).is_err());
    }

    #[test]
    fn python_tags_produce_nested_defs_and_references() {
        let mut extractor = TagsExtractor::new(&PYTHON).unwrap();
        let facts = extractor
            .extract(
                b"class Service:\n    value = 1\n    def run(self):\n        result = helper()\n        def nested():\n            return hidden()\n        return result\n",
            )
            .unwrap();
        assert!(matches!(
            facts.status(),
            ParseStatus::Tags {
                parse_errors: false
            }
        ));
        let method = facts.defs().iter().find(|def| def.name == "run").unwrap();
        assert_eq!(method.local_qualified.as_deref(), Some("Service.run"));
        assert!(method.body_idents.iter().any(|ident| ident == "helper"));
        assert!(!method.body_idents.iter().any(|ident| ident == "hidden"));
        assert!(facts
            .references()
            .iter()
            .any(|reference| reference.name == "helper"));
    }

    #[test]
    fn tags_docs_are_scanned_into_doc_idents_and_out_of_body_idents() {
        let mut extractor = TagsExtractor::new(&PYTHON).unwrap();
        let facts = extractor
            .extract(
                b"def documented():\n    \"\"\"Prose mentions widget.\"\"\"\n    return helper()\n",
            )
            .unwrap();
        let documented = facts.defs().first().unwrap();
        assert!(documented.doc_idents.iter().any(|ident| ident == "widget"));
        assert!(documented.body_idents.iter().any(|ident| ident == "helper"));
        assert!(!documented.body_idents.iter().any(|ident| ident == "widget"));
    }

    #[test]
    fn a_multi_line_header_is_one_signature_and_not_body() {
        let mut extractor = TagsExtractor::new(&PYTHON).unwrap();
        let facts = extractor
            .extract(b"def configure(\n    host,\n    port,\n):\n    return host\n")
            .unwrap();
        let configure = facts.defs().first().unwrap();
        assert_eq!(configure.signature, "def configure( host, port, ):");
        assert!(configure
            .signature_idents
            .iter()
            .any(|ident| ident == "host"));
        assert!(configure
            .signature_idents
            .iter()
            .any(|ident| ident == "port"));
        assert!(!configure.body_idents.iter().any(|ident| ident == "port"));
    }

    #[test]
    fn decorators_are_attributes_of_the_decorated_definition() {
        let mut extractor = TagsExtractor::new(&PYTHON).unwrap();
        let facts = extractor
            .extract(
                b"class Service:\n    @staticmethod\n    def run(value):\n        return value\n",
            )
            .unwrap();
        let run = facts.defs().iter().find(|def| def.name == "run").unwrap();
        assert!(run
            .attribute_idents
            .iter()
            .any(|ident| ident == "staticmethod"));
        assert_eq!(run.signature, "def run(value):");
        assert!(!run.body_idents.iter().any(|ident| ident == "staticmethod"));
        let service = facts
            .defs()
            .iter()
            .find(|def| def.name == "Service")
            .unwrap();
        assert!(!service
            .body_idents
            .iter()
            .any(|ident| ident == "staticmethod"));
    }

    #[test]
    fn python_names_carry_their_conventional_visibility() {
        let mut extractor = TagsExtractor::new(&PYTHON).unwrap();
        let facts = extractor
            .extract(
                b"class Service:\n    def __init__(self):\n        pass\n    def __mangled(self):\n        pass\n    def _hidden(self):\n        pass\n    def run(self):\n        pass\n",
            )
            .unwrap();
        let visibility_of = |name: &str| {
            facts
                .defs()
                .iter()
                .find(|def| def.name == name)
                .unwrap()
                .visibility
                .clone()
        };
        assert_eq!(visibility_of("__init__"), Visibility::Public);
        assert_eq!(visibility_of("__mangled"), Visibility::Private);
        assert_eq!(visibility_of("_hidden"), Visibility::Internal);
        assert_eq!(visibility_of("run"), Visibility::Public);
    }

    #[test]
    fn python_test_conventions_set_test_signals() {
        let mut extractor = TagsExtractor::new(&PYTHON).unwrap();
        let facts = extractor
            .extract(
                b"import pytest\n\nclass TestService:\n    def test_run(self):\n        pass\n\n@pytest.fixture\ndef service():\n    return None\n\ndef plain():\n    pass\n",
            )
            .unwrap();
        let signals_of = |name: &str| {
            facts
                .defs()
                .iter()
                .find(|def| def.name == name)
                .unwrap()
                .test_signals
        };
        assert!(signals_of("test_run").inside_test_scope);
        assert!(signals_of("service").explicit_attribute);
        assert!(!signals_of("plain").any());
    }

    #[test]
    fn malformed_python_is_tags_recovered_not_complete() {
        let mut extractor = TagsExtractor::new(&PYTHON).unwrap();
        let facts = extractor.extract(b"def broken(:\n    pass\n").unwrap();
        assert!(matches!(
            facts.status(),
            ParseStatus::Tags { parse_errors: true }
        ));
    }

    #[test]
    fn invalid_utf8_degrades_lexically() {
        let mut extractor = TagsExtractor::new(&PYTHON).unwrap();
        let facts = extractor.extract(b"def bad(\xff").unwrap();
        assert!(matches!(
            facts.status(),
            ParseStatus::Degraded {
                reason: DegradedReason::InvalidUtf8,
                ..
            }
        ));
    }

    fn typescript(source: &str) -> Facts {
        TagsExtractor::new(&TYPESCRIPT)
            .unwrap()
            .extract(source.as_bytes())
            .unwrap()
    }

    fn kind_of(facts: &Facts, name: &str) -> String {
        facts
            .defs()
            .iter()
            .find(|def| def.name == name)
            .unwrap_or_else(|| panic!("no definition named {name}"))
            .kind
            .to_string()
    }

    /// The three member modifiers no capture can reach, because they are
    /// anonymous tokens the grammar does not give a field to. `refine` reads
    /// them back off the signature text, and only off its leading words — a
    /// `private` in a parameter list or a type is not this class's business.
    #[test]
    fn typescript_member_visibility_comes_off_the_leading_modifiers() {
        let facts = typescript(
            "class C {\n    private a: string;\n    protected b: string;\n    public c: string;\n    d: string;\n    e = (private_thing: string) => 1;\n}\n",
        );
        let visibility_of = |name: &str| {
            &facts
                .defs()
                .iter()
                .find(|def| def.name == name)
                .unwrap()
                .visibility
        };
        assert_eq!(visibility_of("a"), &Visibility::Private);
        assert_eq!(visibility_of("b"), &Visibility::Protected);
        assert_eq!(visibility_of("c"), &Visibility::Public);
        assert_eq!(visibility_of("d"), &Visibility::Public);
        assert_eq!(visibility_of("e"), &Visibility::Public);
    }

    /// A `#` name is private to the runtime rather than to the type checker,
    /// and it is the one visibility TypeScript spells in the name itself.
    #[test]
    fn a_typescript_hash_name_is_private_without_a_modifier() {
        let facts = typescript("class C {\n    #hidden = 0;\n    shown = 0;\n}\n");
        let hidden = facts
            .defs()
            .iter()
            .find(|def| def.name == "#hidden")
            .unwrap();
        assert_eq!(hidden.visibility, Visibility::Private);
        // The sigil is not part of an identifier, so the definition's own name
        // has to be recognised without it or it lands in its own signature.
        assert!(!hidden
            .signature_idents
            .iter()
            .any(|ident| ident == "hidden"));
        assert_eq!(
            facts
                .defs()
                .iter()
                .find(|def| def.name == "shown")
                .unwrap()
                .visibility,
            Visibility::Public
        );
    }

    /// `constructor` and the accessors are `method_definition` nodes like any
    /// other, so the grammar cannot tell them apart and `refine` must.
    #[test]
    fn typescript_constructors_and_accessors_are_not_ordinary_methods() {
        let facts = typescript(
            "class C {\n    constructor() {}\n    get size(): number { return 0; }\n    set size(value: number) {}\n    resize(): void {}\n}\n",
        );
        assert_eq!(kind_of(&facts, "constructor"), "constructor");
        assert_eq!(kind_of(&facts, "resize"), "method");
        let sizes: Vec<_> = facts
            .defs()
            .iter()
            .filter(|def| def.name == "size")
            .map(|def| def.kind.to_string())
            .collect();
        assert_eq!(sizes, vec!["property", "property"]);
    }

    /// An arrow or `function` initializer makes a binding a function; anything
    /// else leaves it the variable it was written as. The signature ends at the
    /// first line break, so an initializer that starts on the next line is not
    /// evidence of anything and is not treated as if it were.
    #[test]
    fn a_typescript_binding_is_refined_only_on_what_its_first_line_shows() {
        let facts = typescript(
            "const arrow = (a: string) => a;\nconst generic = <T,>(a: T): T => a;\nconst plain = function (a: string) { return a; };\nconst value = 3;\nconst wrapped = compute(() => 1);\nconst split =\n    (a: string) => a;\n",
        );
        assert_eq!(kind_of(&facts, "arrow"), "function");
        assert_eq!(kind_of(&facts, "generic"), "function");
        assert_eq!(kind_of(&facts, "plain"), "function");
        assert_eq!(kind_of(&facts, "value"), "variable");
        assert_eq!(kind_of(&facts, "wrapped"), "variable");
        assert_eq!(kind_of(&facts, "split"), "variable");
    }

    /// The two shapes the first `=` and the first token get wrong on their own:
    /// an annotation that spells a function type *before* the assignment, and a
    /// `<` that opens an element rather than a type parameter list.
    #[test]
    fn a_typescript_initializer_is_read_past_its_annotation_and_its_element() {
        let facts = typescript(
            "const annotated: (x: number) => void = (x) => {};\nconst asserted = <Foo>bar.map((y) => y);\nconst nested = <T extends Map<string, number>,>(a: T): T => a;\nconst compared = left >= right;\n",
        );
        assert_eq!(kind_of(&facts, "annotated"), "function");
        assert_eq!(kind_of(&facts, "asserted"), "variable");
        assert_eq!(kind_of(&facts, "nested"), "function");
        assert_eq!(kind_of(&facts, "compared"), "variable");

        // The same `<`, under the grammar where it really does open an element.
        let mut tsx = TagsExtractor::new(&TSX).unwrap();
        let facts = tsx
            .extract(b"const element = <Badge onClick={() => go()} />;\n")
            .unwrap();
        assert_eq!(kind_of(&facts, "element"), "variable");
    }

    /// Nesting is spelled with the separator the language uses, which is the
    /// whole reason [`Lang::qualified_separator`] exists.
    #[test]
    fn typescript_nesting_is_qualified_with_a_dot() {
        let facts = typescript(
            "namespace Outer {\n    export class Inner {\n        run(): void {}\n    }\n}\n",
        );
        let qualified = |name: &str| {
            facts
                .defs()
                .iter()
                .find(|def| def.name == name)
                .unwrap()
                .local_qualified
                .clone()
        };
        assert_eq!(qualified("Inner").as_deref(), Some("Outer.Inner"));
        assert_eq!(qualified("run").as_deref(), Some("Outer.Inner.run"));
    }

    /// A local `const` is not API, and the surface rr publishes would be noise
    /// if every temporary inside every function body landed in it.
    #[test]
    fn a_typescript_binding_inside_a_function_body_is_not_a_definition() {
        let facts = typescript(
            "const exported = 1;\nfunction run(): number {\n    const local = 2;\n    return local + exported;\n}\n",
        );
        assert!(facts.defs().iter().any(|def| def.name == "exported"));
        assert!(!facts.defs().iter().any(|def| def.name == "local"));
    }

    /// A receiver-typed call is one rr cannot resolve without knowing the
    /// receiver's type, so it is recorded as what it is and left for the index
    /// to decline rather than guessed at here.
    #[test]
    fn a_typescript_call_through_a_receiver_is_a_method_reference() {
        let facts = typescript("function run(): void {\n    plain();\n    obj.member();\n}\n");
        let kind_of_reference = |name: &str| {
            &facts
                .references()
                .iter()
                .find(|reference| reference.name == name)
                .unwrap()
                .kind
        };
        assert_eq!(kind_of_reference("plain"), &ReferenceKind::Call);
        assert_eq!(kind_of_reference("member"), &ReferenceKind::MethodCall);
    }

    /// TSX is a second grammar rather than a second file extension, and the
    /// shared query has to compile against both. A `TagsConfiguration` owns the
    /// grammar it was built with, so this is also the test that would fail if
    /// the two specs ever came to share one.
    #[test]
    fn the_shared_query_compiles_against_both_typescript_grammars() {
        let source = "export const Badge = (props: P) => <b>{props.label}</b>;\n";
        let mut tsx = TagsExtractor::new(&TSX).unwrap();
        let facts = tsx.extract(source.as_bytes()).unwrap();
        assert!(matches!(
            facts.status(),
            ParseStatus::Tags {
                parse_errors: false
            }
        ));
        assert_eq!(kind_of(&facts, "Badge"), "function");
        assert!(matches!(
            typescript(source).status(),
            ParseStatus::Tags { parse_errors: true }
        ));
    }
}
