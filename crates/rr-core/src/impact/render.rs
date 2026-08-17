//! The two renderings of one impact report.
//!
//! One value, two projections, and neither one sorts: the order was settled by
//! [`ImpactResultV1::canonicalize`] before either renderer ran, so the text
//! report and the JSON object name the same things in the same order and a
//! reader comparing them never has to.
//!
//! No timestamp, no elapsed time, no absolute path and no colour reaches either
//! output, which is what makes two runs over one comparison byte-identical and
//! the text form diffable in a pull request.
//!
//! # Truncation is the text form's alone
//!
//! `--limit` bounds what a human reads. The JSON object is always complete: it
//! is read by a program that filters for itself, and a truncated object is one a
//! consumer cannot tell from a small one. Where the text form truncates it says
//! `shown 12/419`, so the number it did not print is still in the report.
//!
//! Two things are never truncated. Diagnostics, because a report that hid the
//! reason it is partial would read as complete; and the unfollowed-import note,
//! whose whole purpose is to state a count.

use std::fmt::Write as _;

use super::{
    ChangedDefinition, ChangedFile, EndpointJson, ImpactCycle, ImpactEdge, ImpactNode,
    ImpactResultV1, ImpactStatus, ResolutionCounts, TestImpact, UnfollowedImport, IMPACT_COMMAND,
};
use crate::check::DiagnosticV1;

/// Renders one impact report as the human report.
///
/// `limit` bounds each list independently, in the order
/// [`ImpactResultV1::canonicalize`] left them, so the entries a truncated
/// section shows are the closest ones to the change rather than an arbitrary
/// prefix.
#[must_use]
pub fn render_impact_text(result: &ImpactResultV1, limit: u32) -> String {
    let mut out = format!(
        "impact: {} · base: {} · target: {} · depth: {}\n",
        result.status.as_str(),
        result.base.as_text(),
        result.target.as_text(),
        result.depth
    );

    files(&mut out, &result.changed_files, limit);
    definitions(&mut out, &result.changed_definitions, limit);
    edges(&mut out, &result.direct_edges, limit);
    nodes(&mut out, "affected", &result.affected, limit);
    nodes(&mut out, "dependencies", &result.dependencies, limit);
    cycles(&mut out, &result.cycles, limit);
    tests(&mut out, &result.tests, limit);
    resolution(&mut out, &result.resolution);
    diagnostics(&mut out, &result.diagnostics);
    unfollowed(&mut out, &result.unfollowed_imports, limit);
    out
}

/// Renders one impact report as the compact JSON object.
///
/// Field by field rather than by flattening the result, which is what keeps
/// `schema_version` first and `command` second — the shape every other report
/// surface publishes, and one a `#[serde(flatten)]` would break here in
/// particular, because the result carries `schema_version` itself and would
/// publish it twice. `command` is the renderer's and not the value's: an
/// [`ImpactResultV1`] does not know which invocation printed it.
///
/// # Errors
/// Returns a serialization error from `serde_json`.
pub fn render_impact_json(result: &ImpactResultV1) -> Result<String, serde_json::Error> {
    #[derive(serde::Serialize)]
    struct Envelope<'result> {
        schema_version: u32,
        command: &'static str,
        status: ImpactStatus,
        base: &'result EndpointJson,
        target: &'result EndpointJson,
        depth: u8,
        changed_files: &'result [ChangedFile],
        changed_definitions: &'result [ChangedDefinition],
        direct_edges: &'result [ImpactEdge],
        affected: &'result [ImpactNode],
        dependencies: &'result [ImpactNode],
        cycles: &'result [ImpactCycle],
        tests: &'result [TestImpact],
        resolution: ResolutionCounts,
        diagnostics: &'result [DiagnosticV1],
        unfollowed_imports: &'result [UnfollowedImport],
    }

    serde_json::to_string(&Envelope {
        schema_version: result.schema_version,
        command: IMPACT_COMMAND,
        status: result.status,
        base: &result.base,
        target: &result.target,
        depth: result.depth,
        changed_files: &result.changed_files,
        changed_definitions: &result.changed_definitions,
        direct_edges: &result.direct_edges,
        affected: &result.affected,
        dependencies: &result.dependencies,
        cycles: &result.cycles,
        tests: &result.tests,
        resolution: result.resolution,
        diagnostics: &result.diagnostics,
        unfollowed_imports: &result.unfollowed_imports,
    })
}

/// Writes one section header and answers how many entries follow it.
///
/// An empty section is printed as `none` rather than skipped: a reader who
/// cannot see the heading cannot tell "nothing points at this" from "this
/// version of rr does not report that".
fn header(out: &mut String, label: &str, total: usize, limit: u32) -> usize {
    if total == 0 {
        let _ = writeln!(out, "{label}: none");
        return 0;
    }
    let shown = usize::try_from(limit).unwrap_or(usize::MAX).min(total);
    if shown < total {
        let _ = writeln!(out, "{label} (shown {shown}/{total}):");
    } else {
        let _ = writeln!(out, "{label} ({total}):");
    }
    shown
}

/// The changed files section.
fn files(out: &mut String, entries: &[ChangedFile], limit: u32) {
    let shown = header(out, "changed files", entries.len(), limit);
    for file in entries.iter().take(shown) {
        let _ = write!(
            out,
            "  {} · {} · {} · definitions {}",
            file.path,
            file.endpoint.as_str(),
            file.state.as_str(),
            file.definitions
        );
        if let Some(source) = &file.source {
            let _ = write!(out, " · from {source}");
        }
        out.push('\n');
    }
}

/// The changed definitions section: the seeds every other section is about.
fn definitions(out: &mut String, entries: &[ChangedDefinition], limit: u32) {
    let shown = header(out, "changed definitions", entries.len(), limit);
    for definition in entries.iter().take(shown) {
        let _ = writeln!(
            out,
            "  {} {} · {} · {} · line {}",
            definition.change.as_str(),
            definition.anchor,
            definition.kind.as_str(),
            definition.endpoint.as_str(),
            definition.line
        );
    }
}

/// The direct edges section.
fn edges(out: &mut String, entries: &[ImpactEdge], limit: u32) {
    let shown = header(out, "direct edges", entries.len(), limit);
    for edge in entries.iter().take(shown) {
        let _ = writeln!(
            out,
            "  {} {} -> {} · line {} · {}",
            edge.kind.as_str(),
            edge.source,
            edge.target,
            edge.line,
            edge.endpoint.as_str()
        );
    }
}

/// One closure section, forward or reverse.
fn nodes(out: &mut String, label: &str, entries: &[ImpactNode], limit: u32) {
    let shown = header(out, label, entries.len(), limit);
    for node in entries.iter().take(shown) {
        let _ = writeln!(
            out,
            "  @{} {} · {}",
            node.distance,
            node.anchor,
            node.via.as_str()
        );
    }
}

/// The cycles section.
fn cycles(out: &mut String, entries: &[ImpactCycle], limit: u32) {
    let shown = header(out, "cycles", entries.len(), limit);
    for cycle in entries.iter().take(shown) {
        let _ = writeln!(out, "  @{} {}", cycle.distance, cycle.members.join(" · "));
    }
}

/// The tests section, each file once with every reason it appears.
fn tests(out: &mut String, entries: &[TestImpact], limit: u32) {
    let shown = header(out, "tests", entries.len(), limit);
    for test in entries.iter().take(shown) {
        let reasons: Vec<&str> = test.reasons.iter().map(|reason| reason.as_str()).collect();
        let _ = writeln!(
            out,
            "  {} · {} · {}",
            test.path,
            test.confidence.as_str(),
            reasons.join(", ")
        );
    }
}

/// The resolution counters, always printed in full.
///
/// One line and never truncated, because zeroes are the point: a reader can only
/// trust an empty `affected` section if the counters beside it say nothing was
/// left unresolved.
fn resolution(out: &mut String, counts: &ResolutionCounts) {
    let _ = writeln!(
        out,
        "resolution: exact edges {} · unresolved calls {} · unresolved references {} · unresolved imports {} · ambiguous calls {} · ambiguous references {} · ambiguous imports {} · degraded files {}",
        counts.exact_edges,
        counts.unresolved_calls,
        counts.unresolved_references,
        counts.unresolved_imports,
        counts.ambiguous_calls,
        counts.ambiguous_references,
        counts.ambiguous_imports,
        counts.degraded_files
    );
}

/// The diagnostics section, in [`crate::check`]'s spelling and never truncated.
fn diagnostics(out: &mut String, entries: &[DiagnosticV1]) {
    if entries.is_empty() {
        out.push_str("diagnostics: none\n");
        return;
    }
    let _ = writeln!(out, "diagnostics ({}):", entries.len());
    for diagnostic in entries {
        match &diagnostic.path {
            Some(path) => {
                let _ = writeln!(
                    out,
                    "  {} {} {}: {}",
                    diagnostic.severity.as_str(),
                    diagnostic.rule_id,
                    path,
                    diagnostic.message
                );
            }
            None => {
                let _ = writeln!(
                    out,
                    "  {} {}: {}",
                    diagnostic.severity.as_str(),
                    diagnostic.rule_id,
                    diagnostic.message
                );
            }
        }
        let _ = writeln!(out, "  fix: {}", diagnostic.remediation);
    }
}

/// The unfollowed imports section, and the note that states the whole count.
///
/// The note is printed even when the list was truncated, and it counts every
/// import rather than the shown ones: the sentence exists so that an empty
/// `affected` section cannot be read as "nothing depends on this", and a note
/// that undercounted would do the same damage more quietly.
fn unfollowed(out: &mut String, entries: &[UnfollowedImport], limit: u32) {
    let shown = header(out, "unfollowed imports", entries.len(), limit);
    for import in entries.iter().take(shown) {
        let _ = write!(
            out,
            "  {}:{} · {}",
            import.path, import.line, import.specifier
        );
        if let Some(name) = &import.name {
            let _ = write!(out, " · {name}");
        }
        let _ = writeln!(out, " · {}", import.why);
    }
    if !entries.is_empty() {
        let _ = writeln!(
            out,
            "note: {} import(s) in changed files cannot be followed (this language resolves imports by name, not path)",
            entries.len()
        );
    }
}
