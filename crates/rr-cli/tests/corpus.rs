//! The frozen corpus, from the manifest to the release decision.
//!
//! This file is the harness. It drives the compiled `rr` binary as a black box
//! against a checksum-locked corpus, files what it measured as per-shard
//! evidence, and adjudicates the aggregate against the gates in
//! `rr_core::quality`. It computes no metric of its own: the arithmetic, the
//! gates and the report shape all belong to that module, because a second
//! adjudicator living in a test could disagree with the one `rr check` uses and
//! nobody would find out until a release.
//!
//! # Nothing here is vendored yet, and that is deliberate
//!
//! The three upstream repositories are not in this tree. Copying them needs
//! network access and a redistribution decision — two things a test suite must
//! never take on itself — while corpus tests have to run offline and
//! byte-identically. So [`corpus_gate`], [`corpus_aggregate`] and
//! [`corpus_relock`] are `#[ignore]`d and skip with
//! `rr_core::quality::CORPUS_ABSENT_SKIP` while `fixtures/corpus/manifest.json`
//! is absent, naming the pull request that ends the skip so a skip cannot be
//! read as a passing run. Every other test in this file runs on every `cargo
//! test`, offline, against throwaway corpora it builds itself.
//!
//! # What the corpus never does
//!
//! It runs no hook, no build script and no program other than `rr`. A golden case
//! is arguments in, exit code and bytes out, passed to the binary directly and
//! never to a shell. Fixtures are data.
//!
//! # Sharding, and why the assignment is a hash
//!
//! A case belongs to shard `sha256(case_id) mod` `CORPUS_SHARDS`, read from
//! `RR_CORPUS_SHARD` as `index/total`. The identity decides the shard, not the
//! position in the file, so inserting a case at the top does not move every other
//! case to another machine and invalidate four shard files at once. The
//! aggregation then refuses anything it cannot place — a case nobody ran, a case
//! two shards ran, a case no file declares, a case run by the wrong shard —
//! because each of those changes a denominator, and a denominator that shrank is
//! a score that rose.
//!
//! # Drift is never repaired
//!
//! Ordinary verification cannot write here. Re-locking the manifest and rewriting
//! a golden both require `RR_ACCEPT_CORPUS_UPDATE=1`; without it drift fails and
//! nothing is rewritten. A harness that repaired its own expectations would report
//! a clean run on exactly the change the corpus exists to catch.

#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rr_core::check::DiagnosticV1;
use rr_core::path::RelPath;
use rr_core::quality::{
    adjudicate_gate, aggregate, build_report, generate_synthetic, load_cases, load_manifest,
    load_perf_evidence, materialize_synthetic, relock, routing_metrics, synthetic_digest,
    verify_manifest, AggregationFault, CaseExpectation, CaseKind, CorpusFile, CorpusManifest,
    CorpusRepository, CorpusTier, ExpectedCase, FileMode, GateEvidence, GateFailure, GoldenCase,
    OracleCase, PerfDecision, Provenance, QualityReportV1, Ratio, ReportedRouting, ReportedSafety,
    RoutingCaseVerdict, ShardEvidence, VendoredProvenance, ACCEPT_CORPUS_UPDATE_ENV,
    ACCEPT_CORPUS_UPDATE_VALUE, CORPUS_ABSENT_SKIP, CORPUS_MANIFEST_PATH, CORPUS_ROOT,
    CORPUS_SHARDS, CORPUS_SHARD_ENV, ORACLE_ANSWERABLE_CASES, ORACLE_MIN_ABSTENTION_CASES,
    RR0502_CORPUS_LICENSE_MISMATCH, RR0506_CORPUS_MANIFEST_INVALID, SHARD_EVIDENCE_SCHEMA_VERSION,
    SYNTHETIC_GENERATOR_VERSION, TOP3_GATE_DENOMINATOR, TOP3_GATE_NUMERATOR,
};
use rr_core::render::encode_anchor;
use rr_core::text::Digest;
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

use common::{code, run, stdout};

/// Where a run leaves what it produced, relative to the repository root.
///
/// Under `target/` and not under `fixtures/`: evidence produced by a run is not an
/// input to the next one, and writing it into the corpus would make the very next
/// verification report undeclared files — correctly.
const EVIDENCE_DIR: &str = "target/rr-quality";

/// The report `rr check --quality-report` is pointed at.
const REPORT_NAME: &str = "quality-report-v1.json";

/// What the one pinned platform's benchmark run leaves behind, if it ran.
const PERF_NAME: &str = "perf-evidence-v1.json";

/// The approved top-one recall, in cases, as a corpus-update PR recorded it.
///
/// One integer in one line, and no JSON envelope around it: a schema exists to
/// stop a document being misread, and there is only one way to read a decimal
/// number. Absent means no baseline has been approved yet, so top-one has nothing
/// to regress against and is reported rather than gated.
const TOP1_BASELINE_PATH: &str = "baselines/top1-cases.txt";

/// The repository root, from the crate this test binary was built in.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the crate directory has a workspace root above it")
        .to_path_buf()
}

/// Whether the corpus has been vendored.
///
/// The manifest is the question, not the directory: `fixtures/corpus/` ships with
/// its shape, its README and its case files, and none of that is a corpus until
/// something locks the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CorpusState {
    /// No manifest, so nothing is locked and nothing can be verified.
    Absent,
    /// A manifest is there and the run proceeds.
    Present,
}

fn corpus_state(root: &Path) -> CorpusState {
    if root.join(CORPUS_MANIFEST_PATH).is_file() {
        CorpusState::Present
    } else {
        CorpusState::Absent
    }
}

/// Which shard of the corpus a run was asked to execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShardRequest {
    /// No `RR_CORPUS_SHARD`, so one process runs every case.
    Every,
    /// One shard of `CORPUS_SHARDS`.
    Only(u32),
}

impl ShardRequest {
    /// Whether this request covers the given shard.
    const fn covers(self, shard: u32) -> bool {
        match self {
            Self::Every => true,
            Self::Only(only) => only == shard,
        }
    }

    /// Every shard this request will file evidence for, in order.
    ///
    /// A shard with no case in it still files an empty file. The aggregate
    /// distinguishes "this shard measured nothing" from "this shard never ran",
    /// and it can only do that if a run that measured nothing says so.
    fn shards(self) -> Vec<u32> {
        match self {
            Self::Every => (0..CORPUS_SHARDS).collect(),
            Self::Only(only) => vec![only],
        }
    }
}

/// Why `RR_CORPUS_SHARD` could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShardFault {
    /// Not `index/total`, or not two integers.
    Malformed,
    /// A total this build does not split into.
    ForeignTotal(u32),
    /// An index outside `0..total`.
    OutOfRange(u32),
}

/// Reads `RR_CORPUS_SHARD` as `index/total`.
///
/// The total is spelled out and checked rather than assumed, so a CI matrix that
/// grew to eight jobs fails loudly instead of running half the corpus twice and
/// aggregating the halves it happened to get.
fn parse_shard(raw: Option<&str>) -> Result<ShardRequest, ShardFault> {
    let Some(raw) = raw else {
        return Ok(ShardRequest::Every);
    };
    let Some((index, total)) = raw.split_once('/') else {
        return Err(ShardFault::Malformed);
    };
    let index: u32 = index.parse().map_err(|_| ShardFault::Malformed)?;
    let total: u32 = total.parse().map_err(|_| ShardFault::Malformed)?;
    if total != CORPUS_SHARDS {
        return Err(ShardFault::ForeignTotal(total));
    }
    if index >= total {
        return Err(ShardFault::OutOfRange(index));
    }
    Ok(ShardRequest::Only(index))
}

/// Which shard one case belongs to.
///
/// The whole digest is reduced modulo `CORPUS_SHARDS`, byte by byte, rather than
/// the last byte being taken: `4` divides `256`, so the shortcut agrees today and
/// would silently stop agreeing the day the shard count is not a power of two.
fn shard_of(case_id: &str) -> u32 {
    let digest = Sha256::digest(case_id.as_bytes());
    let mut remainder = 0_u64;
    for byte in digest {
        remainder = (remainder * 256 + u64::from(byte)) % u64::from(CORPUS_SHARDS);
    }
    u32::try_from(remainder).unwrap_or(0)
}

/// Whether this run may rewrite a locked artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateMode {
    /// Drift is a failure and nothing is written. The default, always.
    Refuse,
    /// A corpus-update run may re-lock and rewrite goldens.
    Rewrite,
}

/// Reads `RR_ACCEPT_CORPUS_UPDATE`.
///
/// One accepted spelling and no truthiness: `0`, `false`, `yes` and `true` all
/// mean refuse, because a variable whose every value but one enables writing is a
/// variable that enables writing by accident.
fn update_mode(raw: Option<&str>) -> UpdateMode {
    if raw == Some(ACCEPT_CORPUS_UPDATE_VALUE) {
        UpdateMode::Rewrite
    } else {
        UpdateMode::Refuse
    }
}

/// The frozen corpus, as one run reads it.
struct Corpus {
    /// The lock every byte was checked against.
    manifest: CorpusManifest,
    /// The digest of the manifest's own bytes, which identifies the corpus.
    digest: String,
    /// The oracle's questions, in file order.
    oracle: Vec<OracleCase>,
    /// Every golden case, impact then check, in file order.
    goldens: Vec<GoldenCase>,
}

impl Corpus {
    /// Reads the manifest, the oracle and the goldens, and checks the oracle's
    /// size.
    ///
    /// The size check is here and not in the gate because it is a property of the
    /// corpus rather than of a run: a corpus that grew to 41 answerable questions
    /// and kept the "36 of 40" gate would have quietly lowered it, and the gate's
    /// own `OracleUnderSized` failure only sees the cases a run reported.
    fn open(root: &Path) -> Self {
        let corpus = root.join(CORPUS_ROOT);
        let bytes = std::fs::read(root.join(CORPUS_MANIFEST_PATH)).expect("the manifest is there");
        let manifest =
            load_manifest(&root.join(CORPUS_MANIFEST_PATH)).expect("the manifest decodes");
        let oracle: Vec<OracleCase> =
            load_cases(&corpus.join("queries.json")).expect("the oracle decodes");
        let mut goldens: Vec<GoldenCase> =
            load_cases(&corpus.join("impact-cases.json")).expect("the impact goldens decode");
        goldens.extend(
            load_cases::<GoldenCase>(&corpus.join("check-cases.json"))
                .expect("the check goldens decode"),
        );

        let answerable = oracle
            .iter()
            .filter(|case| case.expectation == CaseExpectation::Answerable)
            .count();
        let abstaining = oracle.len() - answerable;
        assert_eq!(
            u64::try_from(answerable).unwrap_or(u64::MAX),
            ORACLE_ANSWERABLE_CASES,
            "the oracle no longer asks the locked number of answerable questions"
        );
        assert!(
            u64::try_from(abstaining).unwrap_or(0) >= ORACLE_MIN_ABSTENTION_CASES,
            "the oracle carries fewer must-abstain cases than the gate requires"
        );

        Self {
            manifest,
            digest: Digest::of_bytes(&bytes).to_text(),
            oracle,
            goldens,
        }
    }

    /// Every case the corpus declares, with the shard responsible for it.
    ///
    /// Oracle order then golden order, so the aggregate comes out in the corpus's
    /// own order whatever order the shards finished in.
    fn expected_cases(&self) -> Vec<ExpectedCase> {
        let routing = self.oracle.iter().map(|case| ExpectedCase {
            case_id: case.case_id.clone(),
            shard: shard_of(&case.case_id),
            kind: CaseKind::Routing,
        });
        let safety = self.goldens.iter().map(|case| ExpectedCase {
            case_id: case.case_id.clone(),
            shard: shard_of(&case.case_id),
            kind: CaseKind::Safety,
        });
        routing.chain(safety).collect()
    }
}

/// One indexed scratch copy per corpus repository.
///
/// The corpus is read-only, and `rr refresh` writes `.rr/`, `MAP.md` and
/// `SYMBOLS.md`. Those would land under `fixtures/corpus/` as undeclared files, so
/// every case runs against a copy — made once per repository rather than once per
/// case, because indexing the large tier for each of forty questions is the
/// difference between a suite that runs in CI and one nobody runs.
struct Workspace {
    copies: BTreeMap<String, TempDir>,
}

impl Workspace {
    fn new() -> Self {
        Self {
            copies: BTreeMap::new(),
        }
    }

    /// The indexed copy of one repository, made on first use.
    fn repo(&mut self, root: &Path, manifest: &CorpusManifest, id: &str) -> PathBuf {
        if !self.copies.contains_key(id) {
            let copy = TempDir::new().unwrap();
            populate(root, manifest, id, copy.path());
            let refreshed = run(copy.path(), &["refresh"]);
            assert_eq!(code(&refreshed), 0, "rr refresh failed on {id}");
            self.copies.insert(id.to_owned(), copy);
        }
        self.copies[id].path().to_path_buf()
    }
}

/// Puts one repository's bytes into a scratch directory.
///
/// A generated repository is produced from its two numbers rather than copied,
/// because it is not in the tree to copy: the manifest locks its digest, not ten
/// thousand files.
fn populate(root: &Path, manifest: &CorpusManifest, id: &str, into: &Path) {
    let entry = manifest
        .repositories
        .iter()
        .find(|repository| repository.id == id)
        .unwrap_or_else(|| panic!("the corpus declares no repository named {id}"));
    match &entry.provenance {
        Provenance::Generated(generated) => materialize_synthetic(
            into,
            generated.generator_version,
            generated.seed,
            generated.files,
        )
        .expect("the generated repository materializes"),
        Provenance::Vendored(_) => {
            copy_tree(&root.join(CORPUS_ROOT).join("repos").join(id), into);
        }
    }
}

/// Copies one vendored repository into a scratch directory.
///
/// Symbolic links are neither copied nor followed. A verified corpus holds none —
/// `CorpusFault::SymlinkEntry` is exactly that check — and following one would let
/// a link inside the corpus decide what a test run reads.
fn copy_tree(from: &Path, into: &Path) {
    std::fs::create_dir_all(into).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let kind = std::fs::symlink_metadata(entry.path()).unwrap().file_type();
        if kind.is_symlink() {
            continue;
        }
        let target = into.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Runs one oracle question and records what the router did with it.
///
/// The comparison against the accepted and forbidden anchors happens here, in the
/// harness that saw the answer, and only its outcome reaches the metrics. No
/// metric can then quietly re-decide which anchor was acceptable.
fn verdict_of(repo: &Path, case: &OracleCase) -> RoutingCaseVerdict {
    let mut args = vec!["query", case.query.as_str(), "--json"];
    if let Some(path) = &case.path {
        args.push("--path");
        args.push(path.as_str());
    }
    let answer: serde_json::Value = serde_json::from_slice(&run(repo, &args).stdout)
        .unwrap_or_else(|_| panic!("{} did not answer with one JSON object", case.case_id));
    let ranked = ranked_anchors(&answer);
    RoutingCaseVerdict {
        expectation: case.expectation,
        direct_permitted: case.direct_permitted,
        answered_direct: answer.get("result").and_then(serde_json::Value::as_str) == Some("direct"),
        accepted_rank: first_rank(&ranked, &case.accepted),
        forbidden_rank: first_rank(&ranked, &case.forbidden),
    }
}

/// The anchors one answer offers, best first.
///
/// A direct answer is one anchor at rank one; candidates are ranked in the order
/// they were returned; a miss offers none. The spelling comes from
/// `rr_core::render::encode_anchor`, the owner of the anchor format, so a case
/// file is written in the same bytes `rr query` prints.
fn ranked_anchors(answer: &serde_json::Value) -> Vec<String> {
    match answer.get("result").and_then(serde_json::Value::as_str) {
        Some("direct") => answer.get("anchor").map(anchor_text).into_iter().collect(),
        Some("candidates") => answer
            .get("candidates")
            .and_then(serde_json::Value::as_array)
            .map_or_else(Vec::new, |items| {
                items
                    .iter()
                    .filter_map(|item| item.get("anchor"))
                    .map(anchor_text)
                    .collect()
            }),
        _ => Vec::new(),
    }
}

/// One anchor object, in `rr query`'s own anchor spelling.
fn anchor_text(anchor: &serde_json::Value) -> String {
    let path = anchor
        .get("path")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    encode_anchor(
        path,
        anchor.get("symbol").and_then(serde_json::Value::as_str),
    )
}

/// The one-based rank of the first anchor in `wanted`, if any appeared.
fn first_rank(ranked: &[String], wanted: &[String]) -> Option<u32> {
    ranked
        .iter()
        .position(|anchor| wanted.iter().any(|candidate| candidate == anchor))
        .and_then(|index| u32::try_from(index + 1).ok())
}

/// Runs one golden case and says whether it held.
///
/// Under `UpdateMode::Rewrite` the observed bytes replace the golden and the case
/// passes; the manifest then has to be re-locked, which is why a corpus-update run
/// re-locks after writing. Under `UpdateMode::Refuse` — every other run — a
/// mismatch is a failure and the golden is untouched.
fn golden_holds(root: &Path, repo: &Path, case: &GoldenCase, mode: UpdateMode) -> bool {
    let args: Vec<&str> = case.args.iter().map(String::as_str).collect();
    let output = run(repo, &args);
    let observed = stdout(&output);
    let relative = RelPath::new(&case.golden)
        .unwrap_or_else(|_| panic!("{} names a golden outside the corpus", case.case_id));
    let golden = root.join(CORPUS_ROOT).join(relative.as_path());
    if mode == UpdateMode::Rewrite {
        if let Some(parent) = golden.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&golden, &observed).unwrap();
        return true;
    }
    let expected = std::fs::read_to_string(&golden).unwrap_or_default();
    code(&output) == i32::from(case.exit_code) && observed == expected
}

/// Runs every case this shard request covers.
fn measure(root: &Path, corpus: &Corpus, request: ShardRequest) -> Vec<ShardEvidence> {
    let raw = std::env::var(ACCEPT_CORPUS_UPDATE_ENV).ok();
    let mode = update_mode(raw.as_deref());
    let mut workspace = Workspace::new();
    let mut routing: BTreeMap<u32, Vec<ReportedRouting>> = BTreeMap::new();
    let mut safety: BTreeMap<u32, Vec<ReportedSafety>> = BTreeMap::new();

    for case in &corpus.oracle {
        let shard = shard_of(&case.case_id);
        if !request.covers(shard) {
            continue;
        }
        let repo = workspace.repo(root, &corpus.manifest, &case.repo);
        routing.entry(shard).or_default().push(ReportedRouting {
            case_id: case.case_id.clone(),
            verdict: verdict_of(&repo, case),
        });
    }
    for case in &corpus.goldens {
        let shard = shard_of(&case.case_id);
        if !request.covers(shard) {
            continue;
        }
        let repo = workspace.repo(root, &corpus.manifest, &case.repo);
        safety.entry(shard).or_default().push(ReportedSafety {
            case_id: case.case_id.clone(),
            passed: golden_holds(root, &repo, case, mode),
        });
    }

    request
        .shards()
        .into_iter()
        .map(|shard| ShardEvidence {
            schema_version: SHARD_EVIDENCE_SCHEMA_VERSION,
            shard,
            shards: CORPUS_SHARDS,
            manifest_digest: corpus.digest.clone(),
            routing: routing.remove(&shard).unwrap_or_default(),
            safety: safety.remove(&shard).unwrap_or_default(),
        })
        .collect()
}

/// Where one shard files what it measured.
fn shard_path(root: &Path, shard: u32) -> PathBuf {
    root.join(EVIDENCE_DIR).join(format!("shard-{shard}.json"))
}

/// Writes one run's evidence, one file per shard it covered.
fn write_evidence(root: &Path, evidence: &[ShardEvidence]) {
    std::fs::create_dir_all(root.join(EVIDENCE_DIR)).unwrap();
    for shard in evidence {
        std::fs::write(
            shard_path(root, shard.shard),
            document(serde_json::to_string_pretty(shard)),
        )
        .unwrap();
    }
}

/// Reads every shard's evidence, refusing to guess at one that is not there.
///
/// A missing file is reported as `AggregationFault::MissingShard` rather than
/// skipped, through the aggregation itself, so "a shard crashed before writing"
/// and "a shard measured nothing" stay different answers.
fn read_evidence(root: &Path) -> Vec<ShardEvidence> {
    (0..CORPUS_SHARDS)
        .filter_map(|shard| std::fs::read(shard_path(root, shard)).ok())
        .filter_map(|bytes| serde_json::from_slice(&bytes).ok())
        .collect()
}

/// One JSON document as this repository writes them: pretty-printed, because a
/// human reads it in a CI artifact and in a corpus-update diff, and newline
/// terminated like every other committed file here.
///
/// It takes the serializer's own result rather than a generic value, because
/// `rr-cli` depends on `serde_json` and not on `serde`: naming that trait in a
/// signature would add a dependency to this crate for one helper.
fn document(json: serde_json::Result<String>) -> String {
    let mut text = json.expect("the document serializes");
    text.push('\n');
    text
}

/// The performance verdict the pinned platform filed, if it ran.
///
/// `None` is not zero: it leaves `GateEvidence::performance` absent, which blocks
/// under `GateFailure::EvidenceMissing`. A release that did not measure
/// performance has not shown that performance held.
fn perf_decision(root: &Path) -> Option<PerfDecision> {
    load_perf_evidence(&root.join(EVIDENCE_DIR).join(PERF_NAME))
        .ok()
        .map(rr_core::quality::PerfEvidenceV1::decide)
}

/// The approved top-one recall, in cases, if a corpus-update PR recorded one.
fn baseline_top1(root: &Path) -> Option<u64> {
    std::fs::read_to_string(root.join(CORPUS_ROOT).join(TOP1_BASELINE_PATH))
        .ok()
        .and_then(|text| text.trim().parse().ok())
}

/// Every diagnostic, one per line, for a failure message a human can act on.
fn render(diagnostics: &[DiagnosticV1]) -> String {
    use std::fmt::Write as _;

    let mut text = String::new();
    for diagnostic in diagnostics {
        let _ = writeln!(
            text,
            "{} {} {}",
            diagnostic.rule_id,
            diagnostic.path.as_deref().unwrap_or("-"),
            diagnostic.actual.as_deref().unwrap_or("-")
        );
    }
    text
}

/// Runs this shard of the corpus and files what it measured.
#[test]
#[ignore = "needs the vendored corpus; see the corpus-update PR"]
fn corpus_gate() {
    let root = repo_root();
    if corpus_state(&root) == CorpusState::Absent {
        eprintln!("{CORPUS_ABSENT_SKIP}");
        return;
    }
    let drift = verify_manifest(&root);
    assert!(
        drift.is_empty(),
        "the corpus is not the corpus its manifest locks:\n{}",
        render(&drift)
    );
    let raw = std::env::var(CORPUS_SHARD_ENV).ok();
    let request = parse_shard(raw.as_deref())
        .unwrap_or_else(|fault| panic!("{CORPUS_SHARD_ENV} is not index/total: {fault:?}"));
    let corpus = Corpus::open(&root);
    write_evidence(&root, &measure(&root, &corpus, request));
}

/// Adjudicates every shard's evidence against the gates, and files the report.
#[test]
#[ignore = "needs the vendored corpus; see the corpus-update PR"]
fn corpus_aggregate() {
    let root = repo_root();
    if corpus_state(&root) == CorpusState::Absent {
        eprintln!("{CORPUS_ABSENT_SKIP}");
        return;
    }
    let corpus = Corpus::open(&root);
    let shards = read_evidence(&root);
    let aggregated = aggregate(&corpus.expected_cases(), &shards)
        .unwrap_or_else(|fault| panic!("the shards do not add up to one run: {fault}"));
    assert_eq!(
        aggregated.manifest_digest, corpus.digest,
        "the shards measured a corpus other than the one on disk"
    );

    let evidence = GateEvidence {
        routing: Some(routing_metrics(&aggregated.routing)),
        safety: Some(aggregated.safety),
        performance: perf_decision(&root),
        top1_baseline: baseline_top1(&root),
    };
    let decision = adjudicate_gate(&evidence);
    let report = build_report(&aggregated.manifest_digest, &evidence, &decision);
    std::fs::create_dir_all(root.join(EVIDENCE_DIR)).unwrap();
    std::fs::write(
        root.join(EVIDENCE_DIR).join(REPORT_NAME),
        document(serde_json::to_string_pretty(&report)),
    )
    .unwrap();
    assert!(
        !decision.blocked,
        "the corpus blocks this release: {:?}",
        decision
            .failures
            .iter()
            .map(|failure| failure.as_str())
            .collect::<Vec<_>>()
    );
}

/// Re-locks the manifest from the corpus on disk, in a corpus-update run only.
#[test]
#[ignore = "needs the vendored corpus; see the corpus-update PR"]
fn corpus_relock() {
    let root = repo_root();
    if corpus_state(&root) == CorpusState::Absent {
        eprintln!("{CORPUS_ABSENT_SKIP}");
        return;
    }
    let path = root.join(CORPUS_MANIFEST_PATH);
    let previous = load_manifest(&path).expect("the manifest decodes");
    let relocked = relock(&previous, &root).expect("the corpus can be locked");
    if relocked == previous {
        return;
    }
    let raw = std::env::var(ACCEPT_CORPUS_UPDATE_ENV).ok();
    assert_eq!(
        update_mode(raw.as_deref()),
        UpdateMode::Rewrite,
        "the corpus drifted from its manifest; re-lock it in a corpus-update PR with \
         {ACCEPT_CORPUS_UPDATE_ENV}={ACCEPT_CORPUS_UPDATE_VALUE}"
    );
    std::fs::write(&path, document(serde_json::to_string_pretty(&relocked))).unwrap();
}

/// An empty case file, in the shape the case-file contract describes.
const EMPTY_CASES: &str = "{\"schema_version\":1,\"cases\":[]}\n";

/// A digest of the right shape and the wrong value, for a placeholder entry.
const PLACEHOLDER_DIGEST: &str =
    "blake3:0000000000000000000000000000000000000000000000000000000000000000";

/// A throwaway repository root holding one corpus and nothing else.
struct Fixture {
    root: TempDir,
}

impl Fixture {
    /// An empty corpus with the directories the shape declares.
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        for dir in ["repos", "licenses", "adversarial", "baselines"] {
            std::fs::create_dir_all(root.path().join(CORPUS_ROOT).join(dir)).unwrap();
        }
        Self { root }
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    fn corpus(&self) -> PathBuf {
        self.root.path().join(CORPUS_ROOT)
    }

    fn write(&self, path: &str, contents: &str) {
        let full = self.corpus().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, contents).unwrap();
    }

    /// Locks whatever is on disk, carrying `skeleton`'s provenance forward.
    ///
    /// Locked by `rr_core::quality::relock`, the mechanical half of a
    /// corpus-update PR, rather than by a manifest written out here: a fixture
    /// that computed its own digests would be a second implementation of the
    /// thing under test, and it would agree with itself while both were wrong.
    fn seal(&self, skeleton: &CorpusManifest) -> CorpusManifest {
        let locked = relock(skeleton, self.root()).expect("the fixture corpus can be locked");
        self.write(
            "manifest.json",
            &document(serde_json::to_string_pretty(&locked)),
        );
        locked
    }
}

/// A manifest declaring no repository, for a corpus of artifacts only.
fn artifacts_only() -> CorpusManifest {
    CorpusManifest {
        schema_version: 1,
        repositories: Vec::new(),
        artifacts: Vec::new(),
    }
}

/// A manifest declaring one vendored repository under `spdx`.
fn vendored(spdx: &str) -> CorpusManifest {
    CorpusManifest {
        schema_version: 1,
        repositories: vec![CorpusRepository {
            id: String::from("small"),
            tier: CorpusTier::Small,
            provenance: Provenance::Vendored(VendoredProvenance {
                upstream_url: String::from("https://example.invalid/small"),
                commit: String::from("0123456789abcdef0123456789abcdef01234567"),
                retrieved: String::from("2026-01-01"),
                spdx: spdx.to_owned(),
                license_files: vec![placeholder("licenses/small-LICENSE")],
            }),
            files: Vec::new(),
        }],
        artifacts: Vec::new(),
    }
}

/// One declared file whose byte length and digest are not yet the real ones.
fn placeholder(path: &str) -> CorpusFile {
    CorpusFile {
        path: path.to_owned(),
        mode: FileMode::Regular,
        bytes: 0,
        digest: String::from(PLACEHOLDER_DIGEST),
    }
}

/// A sealed corpus holding one vendored repository and its licence.
fn sealed_vendored(spdx: &str) -> (Fixture, CorpusManifest) {
    let fixture = Fixture::new();
    fixture.write("queries.json", EMPTY_CASES);
    fixture.write("repos/small/src/lib.rs", "pub fn small() -> u32 { 1 }\n");
    fixture.write("licenses/small-LICENSE", "MIT License\n");
    let locked = fixture.seal(&vendored(spdx));
    assert!(
        verify_manifest(fixture.root()).is_empty(),
        "a freshly locked corpus verifies"
    );
    (fixture, locked)
}

/// Placeholders are files like any other: exempting a name is how an undeclared
/// payload arrives under that name.
#[test]
fn manifest_rejects_undeclared_file() {
    let fixture = Fixture::new();
    fixture.write("queries.json", EMPTY_CASES);
    fixture.seal(&artifacts_only());
    assert!(verify_manifest(fixture.root()).is_empty());

    fixture.write("adversarial/stray.rs", "fn stray() {}\n");
    let diagnostics = verify_manifest(fixture.root());

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].rule_id, RR0506_CORPUS_MANIFEST_INVALID);
    assert_eq!(diagnostics[0].actual.as_deref(), Some("undeclared-file"));
    assert_eq!(
        diagnostics[0].path.as_deref(),
        Some("fixtures/corpus/adversarial/stray.rs")
    );
}

/// The bytes drifted while the byte length did not, which is the edit a length
/// check alone would forgive.
#[test]
fn manifest_rejects_drifted_checksum() {
    let fixture = Fixture::new();
    fixture.write("baselines/top1-cases.txt", "36\n");
    let locked = fixture.seal(&artifacts_only());
    assert!(verify_manifest(fixture.root()).is_empty());

    fixture.write("baselines/top1-cases.txt", "35\n");
    let diagnostics = verify_manifest(fixture.root());

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].rule_id, RR0506_CORPUS_MANIFEST_INVALID);
    assert_eq!(
        diagnostics[0].expected.as_deref(),
        Some(locked.artifacts[0].digest.as_str())
    );
    assert_eq!(
        diagnostics[0].path.as_deref(),
        Some("fixtures/corpus/baselines/top1-cases.txt")
    );
}

/// `..` and an absolute path are both refused from the string alone, by
/// `RelPath`, the crate's owning path validator — no second parser, and no read
/// attempted at a path that was never inside the corpus.
#[test]
fn manifest_rejects_path_escape() {
    let fixture = Fixture::new();
    fixture.write("queries.json", EMPTY_CASES);
    let mut locked = fixture.seal(&artifacts_only());
    locked.artifacts.push(placeholder("../outside.txt"));
    locked.artifacts.push(placeholder("/etc/passwd"));
    fixture.write(
        "manifest.json",
        &document(serde_json::to_string_pretty(&locked)),
    );

    let diagnostics = verify_manifest(fixture.root());

    let escapes: Vec<&str> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.actual.as_deref() == Some("path-escape"))
        .filter_map(|diagnostic| diagnostic.path.as_deref())
        .collect();
    assert_eq!(
        escapes,
        [
            "fixtures/corpus/../outside.txt",
            "fixtures/corpus//etc/passwd"
        ]
    );
    for diagnostic in &diagnostics {
        assert_eq!(diagnostic.rule_id, RR0506_CORPUS_MANIFEST_INVALID);
    }
}

/// Whether a path *is* a link cannot be answered from a string, so it is asked of
/// the filesystem with `symlink_metadata` and reported apart from a path escape:
/// the two are fixed differently.
#[cfg(unix)]
#[test]
fn manifest_rejects_symlink_entry() {
    let fixture = Fixture::new();
    fixture.write("queries.json", EMPTY_CASES);
    fixture.write("impact-cases.json", EMPTY_CASES);
    fixture.seal(&artifacts_only());
    assert!(verify_manifest(fixture.root()).is_empty());

    let linked = fixture.corpus().join("impact-cases.json");
    std::fs::remove_file(&linked).unwrap();
    std::os::unix::fs::symlink("queries.json", &linked).unwrap();
    let diagnostics = verify_manifest(fixture.root());

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].rule_id, RR0506_CORPUS_MANIFEST_INVALID);
    assert_eq!(diagnostics[0].actual.as_deref(), Some("symlink-entry"));
}

/// An unlisted SPDX expression and a licence file that is not there are both
/// licence questions before they are checksum questions: what a release needs to
/// know is whether rr may still redistribute these bytes.
#[test]
fn license_mismatch_is_rr0502() {
    let (fixture, mut locked) = sealed_vendored("MIT");
    if let Provenance::Vendored(vendored) = &mut locked.repositories[0].provenance {
        vendored.spdx = String::from("WTFPL");
    }
    fixture.write(
        "manifest.json",
        &document(serde_json::to_string_pretty(&locked)),
    );
    let unlisted = verify_manifest(fixture.root());

    assert_eq!(unlisted.len(), 1);
    assert_eq!(unlisted[0].rule_id, RR0502_CORPUS_LICENSE_MISMATCH);
    assert_eq!(unlisted[0].actual.as_deref(), Some("WTFPL"));

    let (fixture, _) = sealed_vendored("MIT");
    std::fs::remove_file(fixture.corpus().join("licenses/small-LICENSE")).unwrap();
    let absent = verify_manifest(fixture.root());

    assert_eq!(absent.len(), 1);
    assert_eq!(absent[0].rule_id, RR0502_CORPUS_LICENSE_MISMATCH);
    assert_eq!(absent[0].actual.as_deref(), Some("missing-file"));
}

/// The tier is the only thing that says which vendored tree a figure was measured
/// on, so two repositories claiming one tier means a tier is missing — and every
/// figure filed under the missing one was measured somewhere else.
#[test]
fn manifest_rejects_two_repositories_at_one_tier() {
    let fixture = Fixture::new();
    fixture.write("queries.json", EMPTY_CASES);
    fixture.write("repos/small/src/lib.rs", "pub fn small() -> u32 { 1 }\n");
    fixture.write("repos/twin/src/lib.rs", "pub fn twin() -> u32 { 2 }\n");
    fixture.write("licenses/small-LICENSE", "MIT License\n");
    fixture.write("licenses/twin-LICENSE", "MIT License, again\n");
    let mut skeleton = vendored("MIT");
    let mut twin = skeleton.repositories[0].clone();
    twin.id = String::from("twin");
    if let Provenance::Vendored(vendored) = &mut twin.provenance {
        vendored.license_files = vec![placeholder("licenses/twin-LICENSE")];
    }
    skeleton.repositories.push(twin);
    fixture.seal(&skeleton);

    let diagnostics = verify_manifest(fixture.root());

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].rule_id, RR0506_CORPUS_MANIFEST_INVALID);
    assert_eq!(
        diagnostics[0].expected.as_deref(),
        Some("one repository at tier small")
    );
    assert_eq!(diagnostics[0].actual.as_deref(), Some("small, twin"));
}

/// Two numbers reproduce the repository, byte for byte, on any machine — which is
/// what lets the manifest lock one digest instead of ten thousand files.
#[test]
fn synthetic_generator_is_byte_deterministic_for_a_seed() {
    let seed = 424_242;
    let files = 24;
    let first = generate_synthetic(SYNTHETIC_GENERATOR_VERSION, seed, files).unwrap();
    assert_eq!(
        first,
        generate_synthetic(SYNTHETIC_GENERATOR_VERSION, seed, files).unwrap()
    );

    let here = TempDir::new().unwrap();
    let there = TempDir::new().unwrap();
    for target in [here.path(), there.path()] {
        materialize_synthetic(target, SYNTHETIC_GENERATOR_VERSION, seed, files).unwrap();
    }
    for file in &first {
        let written = std::fs::read_to_string(here.path().join(&file.path)).unwrap();
        assert_eq!(written, file.contents);
        assert_eq!(
            written,
            std::fs::read_to_string(there.path().join(&file.path)).unwrap()
        );
    }

    let digest = synthetic_digest(SYNTHETIC_GENERATOR_VERSION, seed, files).unwrap();
    assert_eq!(
        digest,
        synthetic_digest(SYNTHETIC_GENERATOR_VERSION, seed, files).unwrap()
    );
    assert_ne!(
        digest,
        synthetic_digest(SYNTHETIC_GENERATOR_VERSION, seed + 1, files).unwrap()
    );
}

/// The identity decides the shard, so inserting a case at the top of a file does
/// not move every other case to another machine. The four pinned values are the
/// assignment itself: recomputing them from the code under test would pin nothing.
#[test]
fn shard_assignment_is_stable_across_runs() {
    for (case_id, shard) in [
        ("safety-corruption-03", 0),
        ("check-stale-01", 1),
        ("query-alias-001", 2),
        ("impact-row-01", 3),
    ] {
        assert_eq!(shard_of(case_id), shard, "{case_id}");
        assert_eq!(shard_of(case_id), shard_of(case_id));
    }
    for index in 0..64 {
        assert!(shard_of(&format!("case-{index:03}")) < CORPUS_SHARDS);
    }
    assert_eq!(parse_shard(None), Ok(ShardRequest::Every));
    assert_eq!(parse_shard(Some("2/4")), Ok(ShardRequest::Only(2)));
    assert_eq!(parse_shard(Some("4/4")), Err(ShardFault::OutOfRange(4)));
    assert_eq!(parse_shard(Some("0/8")), Err(ShardFault::ForeignTotal(8)));
    assert_eq!(parse_shard(Some("0")), Err(ShardFault::Malformed));
}

/// One routing case, filed by the shard the assignment gives it.
fn reported(case_id: &str, verdict: RoutingCaseVerdict) -> ReportedRouting {
    ReportedRouting {
        case_id: case_id.to_owned(),
        verdict,
    }
}

/// A routing case the corpus declares.
fn declared(case_id: &str) -> ExpectedCase {
    ExpectedCase {
        case_id: case_id.to_owned(),
        shard: shard_of(case_id),
        kind: CaseKind::Routing,
    }
}

/// One shard filing the cases assigned to it and nothing else.
fn shard_of_cases(shard: u32, cases: &[&str]) -> ShardEvidence {
    ShardEvidence {
        schema_version: SHARD_EVIDENCE_SCHEMA_VERSION,
        shard,
        shards: CORPUS_SHARDS,
        manifest_digest: String::from(PLACEHOLDER_DIGEST),
        routing: cases
            .iter()
            .copied()
            .filter(|case_id| shard_of(case_id) == shard)
            .map(|case_id| reported(case_id, answerable(Some(1), true)))
            .collect(),
        safety: Vec::new(),
    }
}

/// The four case identities that fall one per shard, so a fixture can cover every
/// shard without a case that belongs to two.
const ONE_PER_SHARD: [&str; 4] = [
    "safety-corruption-03",
    "check-stale-01",
    "query-alias-001",
    "impact-row-01",
];

/// A duplicate does not merely double-count: it is a case whose second verdict
/// silently replaces the first, and a run that reported a case twice did not
/// report some other case at all.
#[test]
fn aggregation_rejects_a_duplicate_case() {
    let expected: Vec<ExpectedCase> = ONE_PER_SHARD.iter().copied().map(declared).collect();
    let mut shards: Vec<ShardEvidence> = (0..CORPUS_SHARDS)
        .map(|shard| shard_of_cases(shard, &ONE_PER_SHARD))
        .collect();
    assert!(aggregate(&expected, &shards).is_ok());

    let doubled = "query-alias-001";
    shards[usize::try_from(shard_of(doubled)).unwrap()]
        .routing
        .push(reported(doubled, answerable(Some(1), true)));

    assert_eq!(
        aggregate(&expected, &shards),
        Err(AggregationFault::DuplicateCase(doubled.to_owned()))
    );
}

/// A shard that never filed evidence is not an empty shard. Aggregating three of
/// four would compute a smaller denominator and publish a higher score than the
/// run earned.
#[test]
fn aggregation_rejects_a_missing_shard() {
    let expected: Vec<ExpectedCase> = ONE_PER_SHARD.iter().copied().map(declared).collect();
    let mut shards: Vec<ShardEvidence> = (0..CORPUS_SHARDS)
        .map(|shard| shard_of_cases(shard, &ONE_PER_SHARD))
        .collect();
    shards.retain(|shard| shard.shard != 2);

    assert_eq!(
        aggregate(&expected, &shards),
        Err(AggregationFault::MissingShard(2))
    );
}

/// A case no case file declares is evidence about something the corpus does not
/// ask, and admitting it would let a shard invent its own questions.
#[test]
fn aggregation_rejects_an_undeclared_case() {
    let expected: Vec<ExpectedCase> = ONE_PER_SHARD.iter().copied().map(declared).collect();
    let mut shards: Vec<ShardEvidence> = (0..CORPUS_SHARDS)
        .map(|shard| shard_of_cases(shard, &ONE_PER_SHARD))
        .collect();
    let stranger = "query-invented-999";
    shards[usize::try_from(shard_of(stranger)).unwrap()]
        .routing
        .push(reported(stranger, answerable(Some(1), true)));

    assert_eq!(
        aggregate(&expected, &shards),
        Err(AggregationFault::UnknownCase(stranger.to_owned()))
    );
}

/// A case run by the wrong shard is a case that was also skipped by the right
/// one, so the aggregate names the mismatch instead of counting it.
#[test]
fn aggregation_rejects_a_case_run_by_the_wrong_shard() {
    let expected: Vec<ExpectedCase> = ONE_PER_SHARD.iter().copied().map(declared).collect();
    let mut shards: Vec<ShardEvidence> = (0..CORPUS_SHARDS)
        .map(|shard| shard_of_cases(shard, &ONE_PER_SHARD))
        .collect();
    let strayed = "impact-row-01";
    shards[0]
        .routing
        .push(reported(strayed, answerable(Some(1), true)));

    assert_eq!(
        aggregate(&expected, &shards),
        Err(AggregationFault::MisshardedCase {
            case_id: strayed.to_owned(),
            ran_on: 0,
            belongs_to: shard_of(strayed),
        })
    );
}

/// One answerable case, as the harness files it.
fn answerable(accepted_rank: Option<u32>, answered_direct: bool) -> RoutingCaseVerdict {
    RoutingCaseVerdict {
        expectation: CaseExpectation::Answerable,
        direct_permitted: true,
        answered_direct,
        accepted_rank,
        forbidden_rank: None,
    }
}

/// One must-abstain case that abstained.
fn abstained() -> RoutingCaseVerdict {
    RoutingCaseVerdict {
        expectation: CaseExpectation::MustAbstain,
        direct_permitted: false,
        answered_direct: false,
        accepted_rank: None,
        forbidden_rank: None,
    }
}

/// How many answerable cases the oracle asks, as a `usize`.
fn answerable_count() -> usize {
    usize::try_from(ORACLE_ANSWERABLE_CASES).unwrap()
}

/// How many must-abstain cases the oracle carries, as a `usize`.
fn abstention_count() -> usize {
    usize::try_from(ORACLE_MIN_ABSTENTION_CASES).unwrap()
}

/// A run in which `hits` of the answerable cases were answered at rank one and
/// every must-abstain case abstained.
fn run_of(hits: usize, direct: bool) -> Vec<RoutingCaseVerdict> {
    let mut verdicts = vec![answerable(Some(1), direct); hits];
    verdicts.resize(answerable_count(), answerable(None, false));
    verdicts.resize(answerable_count() + abstention_count(), abstained());
    verdicts
}

/// Evidence that clears every gate except whatever the caller changed.
fn evidence_of(verdicts: &[RoutingCaseVerdict]) -> GateEvidence {
    GateEvidence {
        routing: Some(routing_metrics(verdicts)),
        safety: Some(Ratio::new(12, 12)),
        performance: Some(PerfDecision::decide(
            Ratio::new(100, 100),
            Ratio::new(100, 100),
        )),
        top1_baseline: None,
    }
}

/// One wrong direct answer is enough, however high the rest of the run scored: a
/// direct anchor sends an agent to edit that file and nothing else.
#[test]
fn gate_blocks_on_one_false_direct() {
    let mut verdicts = run_of(answerable_count(), true);
    verdicts[0] = answerable(Some(2), true);
    let metrics = routing_metrics(&verdicts);
    assert_eq!(metrics.false_directs, 1);
    assert!(metrics.top3.is_whole());

    let decision = adjudicate_gate(&evidence_of(&verdicts));

    assert!(decision.blocked);
    assert_eq!(
        decision.failures,
        vec![
            GateFailure::FalseDirect,
            GateFailure::DirectPrecisionBelowOne
        ]
    );
}

/// The gate is 36 of 40 and it is compared on the integers: 35 of 40 rounds to
/// `0.875000` and is below it.
#[test]
fn gate_blocks_when_top3_is_35_of_40() {
    let verdicts = run_of(answerable_count() - 5, false);
    let metrics = routing_metrics(&verdicts);
    assert_eq!(metrics.top3.numerator, TOP3_GATE_NUMERATOR - 1);
    assert_eq!(metrics.top3.denominator, TOP3_GATE_DENOMINATOR);

    let decision = adjudicate_gate(&evidence_of(&verdicts));

    assert!(decision.blocked);
    assert_eq!(decision.failures, vec![GateFailure::Top3BelowGate]);
}

/// Exactly at the gate passes. A gate written as "at least" that failed at its own
/// threshold would be a different, undocumented gate.
#[test]
fn gate_passes_at_exactly_36_of_40() {
    let verdicts = run_of(answerable_count() - 4, true);
    let metrics = routing_metrics(&verdicts);
    assert_eq!(metrics.top3.numerator, TOP3_GATE_NUMERATOR);
    assert_eq!(metrics.top3.denominator, TOP3_GATE_DENOMINATOR);
    assert_eq!(metrics.top3.to_text(), "0.900000");

    let decision = adjudicate_gate(&evidence_of(&verdicts));

    assert!(!decision.blocked, "{:?}", decision.failures);
}

/// The must-abstain cases are counted in the abstention denominators and nowhere
/// else. Folding them into the answerable denominator would let eight guaranteed
/// abstentions raise a recall figure they never participated in.
#[test]
fn abstention_cases_do_not_inflate_the_denominator() {
    let answerable_only = run_of(answerable_count() - 4, true)
        .into_iter()
        .take(answerable_count())
        .collect::<Vec<_>>();
    let with_abstainers = run_of(answerable_count() - 4, true);

    let narrow = routing_metrics(&answerable_only);
    let wide = routing_metrics(&with_abstainers);

    assert_eq!(narrow.top3, wide.top3);
    assert_eq!(narrow.top1, wide.top1);
    assert_eq!(narrow.direct_precision, wide.direct_precision);
    assert_eq!(wide.top3.denominator, ORACLE_ANSWERABLE_CASES);
    assert_eq!(
        wide.abstention.denominator,
        ORACLE_ANSWERABLE_CASES + ORACLE_MIN_ABSTENTION_CASES
    );
    assert_eq!(
        wide.required_abstention.denominator,
        ORACLE_MIN_ABSTENTION_CASES
    );
    assert!(wide.required_abstention.is_whole());
}

/// Both bounds, or neither. A point estimate alone blocks on noise; a confidence
/// bound alone blocks on a difference too small to act on. And neither is a
/// duration: the verdict is a ratio against a baseline this repository approved.
#[test]
fn regression_needs_both_the_point_estimate_and_the_ci_bound() {
    let verdicts = run_of(answerable_count(), true);
    let noisy = GateEvidence {
        performance: Some(PerfDecision::decide(
            Ratio::new(130, 100),
            Ratio::new(102, 100),
        )),
        ..evidence_of(&verdicts)
    };
    let small = GateEvidence {
        performance: Some(PerfDecision::decide(
            Ratio::new(106, 100),
            Ratio::new(106, 100),
        )),
        ..evidence_of(&verdicts)
    };
    let real = GateEvidence {
        performance: Some(PerfDecision::decide(
            Ratio::new(131, 100),
            Ratio::new(106, 100),
        )),
        ..evidence_of(&verdicts)
    };

    assert!(!adjudicate_gate(&noisy).blocked);
    assert!(!adjudicate_gate(&small).blocked);
    let decision = adjudicate_gate(&real);
    assert_eq!(decision.failures, vec![GateFailure::PerformanceRegression]);

    let report = build_report(PLACEHOLDER_DIGEST, &real, &decision);
    assert_eq!(report.findings.len(), 1);
    assert!(report.findings[0].blocked);
    assert!(!document(serde_json::to_string_pretty(&report)).contains("ms"));
}

/// Absence is a skip with a named reason and never a pass, and never a red build
/// for work nobody has been asked to do: vendoring needs network access and a
/// licence decision, so it is a human step.
#[test]
fn corpus_gate_skips_cleanly_without_a_manifest() {
    let fixture = Fixture::new();

    assert_eq!(corpus_state(fixture.root()), CorpusState::Absent);
    assert!(verify_manifest(fixture.root()).is_empty());

    fixture.write(
        "manifest.json",
        &document(serde_json::to_string_pretty(&artifacts_only())),
    );
    assert_eq!(corpus_state(fixture.root()), CorpusState::Present);

    assert_eq!(
        CORPUS_ABSENT_SKIP,
        "fixtures/corpus/manifest.json absent; see the corpus-update PR"
    );
    assert!(CORPUS_ABSENT_SKIP.contains(CORPUS_MANIFEST_PATH));
}

/// Drift never repairs itself. A harness that rewrote its own expectations would
/// report a clean run on exactly the change the corpus exists to catch, so the one
/// mode that may write demands one exact spelling.
#[test]
fn update_mode_requires_the_env_var() {
    assert_eq!(update_mode(None), UpdateMode::Refuse);
    for refused in ["", "0", "no", "false", "true", "yes", "2", " 1", "1 "] {
        assert_eq!(
            update_mode(Some(refused)),
            UpdateMode::Refuse,
            "{refused:?} enabled a rewrite"
        );
    }
    assert_eq!(
        update_mode(Some(ACCEPT_CORPUS_UPDATE_VALUE)),
        UpdateMode::Rewrite
    );
    assert_eq!(ACCEPT_CORPUS_UPDATE_ENV, "RR_ACCEPT_CORPUS_UPDATE");
}

/// The report a corpus run files is decoded by the module that reads it, not by a
/// second reader written here: one shape, one strict decoder, one contract.
#[test]
fn the_report_a_run_files_is_the_report_rr_check_reads() {
    let verdicts = run_of(answerable_count() - 5, false);
    let evidence = evidence_of(&verdicts);
    let decision = adjudicate_gate(&evidence);
    let report = build_report(PLACEHOLDER_DIGEST, &evidence, &decision);

    let round_tripped: QualityReportV1 =
        serde_json::from_str(&document(serde_json::to_string_pretty(&report))).unwrap();

    assert_eq!(round_tripped, report);
    assert_eq!(round_tripped.manifest_digest, PLACEHOLDER_DIGEST);
    assert_eq!(round_tripped.findings.len(), 1);
}
