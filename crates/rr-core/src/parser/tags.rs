//! Generic facts extraction from a grammar's `queries/tags.scm`.

use std::ops::Range;

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
    pub visibility: Visibility,
    pub separator: &'static str,
}

/// Stateful generic tags extractor: one tags parser/context per worker.
pub struct TagsExtractor {
    spec: &'static LanguageSpec,
    config: TagsConfiguration,
    context: TagsContext,
}

impl TagsExtractor {
    /// Compiles and validates one language's tags query.
    ///
    /// # Errors
    /// Returns a construction error when the query is invalid or contains a
    /// definition/reference capture with no kind mapping.
    pub fn new(spec: &'static LanguageSpec) -> std::result::Result<Self, String> {
        let language: tree_sitter::Language = spec.language.into();
        let config = TagsConfiguration::new(language, spec.tags_query, spec.locals_query)
            .map_err(|error| error.to_string())?;
        validate_kind_maps(spec, &config)?;
        Ok(Self {
            spec,
            config,
            context: TagsContext::new(),
        })
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
        let config = &self.config;
        let Ok((tags, parse_errors)) = self.context.generate_tags(config, content, None) else {
            return Ok(degraded_facts(content, DegradedReason::ParserReturnedNone));
        };

        let mut defs = Vec::new();
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
                let (signature_span, signature, signature_idents) =
                    signature_for(span, name, source, &lines)?;
                let doc_idents = tag.docs.as_deref().map(scan_idents).unwrap_or_default();
                defs.push(Def {
                    name: name.to_owned(),
                    local_qualified: None,
                    kind,
                    visibility: spec.visibility.clone(),
                    span,
                    signature_span,
                    signature,
                    signature_idents,
                    body_idents: Vec::new(),
                    doc_idents,
                    attribute_idents: Vec::new(),
                    test_signals: TestSignals::default(),
                });
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

        defs.sort_by(|left, right| def_key(left).cmp(&def_key(right)));
        defs.dedup_by(|left, right| def_key(left) == def_key(right));
        assign_nesting(&mut defs, source, spec.separator);

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

fn signature_for(
    span: Span,
    name: &str,
    source: &str,
    lines: &LineIndex,
) -> Result<(Span, String, Vec<String>)> {
    let start = span.start_byte() as usize;
    let end = span.end_byte() as usize;
    let first_line_end = source
        .get(start..end)
        .and_then(|text| text.find('\n').map(|offset| start + offset))
        .unwrap_or(end);
    let mut signature_end = first_line_end.min(start.saturating_add(MAX_SIGNATURE_BYTES));
    while signature_end > start && !source.is_char_boundary(signature_end) {
        signature_end -= 1;
    }
    let signature_span = span_for_range(&(start..signature_end), lines, source)?;
    let raw = source.get(start..signature_end).unwrap_or_default();
    let signature = {
        let displayed = crate::facts::display_signature(raw);
        if displayed.is_empty() {
            name.to_owned()
        } else {
            displayed
        }
    };
    let signature_idents = scan_idents(raw)
        .into_iter()
        .filter(|ident| ident != name)
        .collect();
    Ok((signature_span, signature, signature_idents))
}

fn assign_nesting(defs: &mut [Def], source: &str, separator: &str) {
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
        stack.push(index);
    }

    for (index, def) in defs.iter_mut().enumerate() {
        let mut exclusions = Vec::with_capacity(direct_children[index].len() + 1);
        exclusions.push(def.signature_span);
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

pub(crate) static PYTHON: LanguageSpec = LanguageSpec {
    lang: Lang::Python,
    language: tree_sitter_python::LANGUAGE,
    tags_query: tree_sitter_python::TAGS_QUERY,
    locals_query: "",
    kinds: &[
        ("constant", DefKind::Variable),
        ("class", DefKind::Class),
        ("function", DefKind::Function),
    ],
    reference_kinds: &[("call", ReferenceKind::Call)],
    visibility: Visibility::Public,
    separator: ".",
};

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
            visibility: Visibility::Public,
            separator: ".",
        };
        static INCOMPLETE: LanguageSpec = LanguageSpec {
            lang: Lang::Python,
            language: tree_sitter_python::LANGUAGE,
            tags_query: "(module) @definition.module",
            locals_query: "",
            kinds: &[],
            reference_kinds: &[],
            visibility: Visibility::Public,
            separator: ".",
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
    fn tags_docs_are_scanned_into_doc_idents() {
        static DOCUMENTED: LanguageSpec = LanguageSpec {
            lang: Lang::Python,
            language: tree_sitter_python::LANGUAGE,
            tags_query: "(function_definition name: (identifier) @name body: (block) @doc) @definition.function",
            locals_query: "",
            kinds: &[("function", DefKind::Function)],
            reference_kinds: &[],
            visibility: Visibility::Public,
            separator: ".",
        };
        let mut extractor = TagsExtractor::new(&DOCUMENTED).unwrap();
        let facts = extractor
            .extract(b"def documented():\n    return helper()\n")
            .unwrap();
        let documented = facts.defs().first().unwrap();
        assert!(documented.doc_idents.iter().any(|ident| ident == "helper"));
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
}
