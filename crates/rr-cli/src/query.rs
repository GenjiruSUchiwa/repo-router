//! The `rr query` command: route once, optionally verify source, render once.
//!
//! Verification is a strict sequence and every step can only refuse or fail
//! closed, so it lives here as one readable function rather than spread across
//! the renderer or the acquisition layer. `rr-core` decides *what* the answer
//! is; `rr-git` decides *what the file says*; this module is the only place
//! that puts the two together.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context};
use rr_core::cancel::CancelToken;
use rr_core::index::Snapshot;
use rr_core::path::RelPath;
use rr_core::query::{parse_query, resolve_route_anchor, route_query, QueryRequest};
use rr_core::ranking::{RankingScratch, DEFAULT_RANKING_PROFILE};
use rr_core::refresh::SnapshotLabel;
use rr_core::render::{
    encode_anchor, render_json, render_json_explained, render_text, render_text_explained,
};
use rr_core::result::{resolve_anchor, Candidate, Pipeline, QueryResult, TargetId};
use rr_core::snapshot::{LoadOutcome, SnapshotStore};
use rr_core::text::{
    api_identity, encode_map_destination, load_routes, projected_map_catalog, update_routes,
    RouteKey, RouteRecord, DEFAULT_MAP_BUDGET,
};
use rr_core::verify::{
    finish_source, resolve_indexed_source, verify_source, PendingSource, SourceResult,
};
use rr_git::{acquire_for_source, revalidate_source, AcquireOutcome, GitRepo};

use crate::output::Output;

#[derive(clap::Args, Debug)]
pub struct Args {
    #[arg(long, value_name = "REL_PATH")]
    path: Option<RelPath>,
    #[arg(long)]
    json: bool,
    /// Report the work the ranker did, including whether the candidate cap
    /// discarded members it could not tell apart from the ones it kept.
    #[arg(long)]
    explain: bool,
    /// Return the anchor's own source, verified against the indexed content
    /// identity, when routing lands on exactly one direct anchor.
    #[arg(long)]
    source: bool,
    query: String,
}

/// Runs one query and returns its exit code.
///
/// # Errors
/// Returns an execution error for repository, snapshot, and I/O failures.
/// Expected worktree states are refusals inside the result, not errors here.
pub fn run(args: &Args) -> anyhow::Result<u8> {
    let workspace = Workspace::open()?;

    let request = QueryRequest::new(&args.query, args.path.as_ref());
    let parsed = parse_query(&workspace.snapshot, request).map_err(anyhow::Error::new)?;
    let mut scratch = RankingScratch::new();

    // A cache hit is only a shortcut, so it must cost less than the work it
    // skips: `route_lookup` reads two small files and resolves one anchor, while
    // `route_query` on a miss ranks the whole corpus. The projection that names
    // the corpus identity is paid once per published snapshot, by the miss that
    // learns a route and not by the hits that follow. A `--path` query is
    // neither read nor learned — the qualifier is part of the question and is
    // not part of the key, so caching one would let a later unqualified
    // question receive a qualified answer.
    let key = args
        .path
        .is_none()
        .then(|| RouteKey::new(&args.query))
        .flatten();
    let cached = key.as_ref().and_then(|key| route_lookup(&workspace, key));

    let (mut result, evidence) = match cached {
        Some(candidate) => (
            QueryResult::Direct {
                candidate,
                pipeline: Pipeline::Route,
                source: None,
            },
            None,
        ),
        None => route_query(
            &workspace.snapshot,
            &parsed,
            &DEFAULT_RANKING_PROFILE,
            &mut scratch,
        )
        .map_err(anyhow::Error::new)?,
    };

    if cached.is_none() {
        learn_route(&workspace, key.as_ref(), &result);
    }

    if args.source {
        attach_source(&workspace, &mut result)?;
    }

    let rendered = match (args.json, args.explain) {
        (true, false) => render_json(&workspace.snapshot, &result),
        (true, true) => render_json_explained(&workspace.snapshot, &result, evidence.as_ref()),
        (false, false) => render_text(&workspace.snapshot, &result),
        (false, true) => render_text_explained(&workspace.snapshot, &result, evidence.as_ref()),
    }
    .map_err(|err| anyhow::anyhow!("{err}"))?;

    Output::print_raw(&rendered).context("write query result")?;
    Ok(result.exit_code())
}

/// The repository and snapshot a query runs against, already agreed with each
/// other: a snapshot built elsewhere never reaches routing.
struct Workspace {
    repo: Option<GitRepo>,
    root: PathBuf,
    snapshot: Arc<Snapshot>,
}

impl Workspace {
    fn open() -> anyhow::Result<Self> {
        let current_dir = std::env::current_dir().context("resolve current directory")?;
        let canonical = current_dir
            .canonicalize()
            .context("canonicalize current directory")?;
        let repo = GitRepo::discover(&canonical).context("discover repository")?;

        let (root, head_oid) = match &repo {
            Some(repo) => (
                repo.workdir().to_path_buf(),
                repo.head_oid().context("resolve HEAD commit")?,
            ),
            None => (canonical, None),
        };

        let snapshot = match SnapshotStore::new(&root)
            .load()
            .map_err(|err| anyhow::anyhow!("{err}"))?
        {
            LoadOutcome::Ready(snapshot) => snapshot,
            LoadOutcome::Missing => bail!("index missing; run 'rr map'"),
            LoadOutcome::NeedsRebuild(_) => bail!("index invalid; run 'rr map'"),
        };

        if repo.is_some() {
            if snapshot.meta.no_git {
                bail!("index repository mismatch; run 'rr map'");
            }
            if snapshot.meta.repo_head_oid != head_oid && !index_still_holds(&root, head_oid)? {
                bail!("index is stale; run 'rr refresh'");
            }
        } else if !snapshot.meta.no_git {
            bail!("index repository mismatch; run 'rr map'");
        }

        Ok(Self {
            repo,
            root,
            snapshot,
        })
    }
}

/// Whether the snapshot still describes the repository, asked properly.
///
/// A moved `HEAD` is not a stale index. The commit that moves it most often is
/// the one that commits the generated maps, which touches no indexed source at
/// all — and `rr status` already knows that, because #44 gave it the
/// `HEAD`-to-`HEAD` tree diff. Comparing commit ids here instead left the two
/// commands contradicting each other on the same snapshot in the same second:
/// `snapshot: fresh, stale_paths: 0` from one, `index is stale` from the other.
/// So this asks `rr status`'s own question rather than a cheaper one that gives
/// a different answer.
///
/// Only reached when the ids differ, and then **once**. The window this opens is
/// the one a user meets first — `rr map`, commit the generated maps, ask
/// something — and it lasts until the next `rr refresh`, so a walk per query
/// would make every query in it slow rather than the first. The answer is
/// therefore memoized against the commit it was reached for.
///
/// The memo is stamped against the snapshot file, so a rebuilt index is never
/// answered from it. Within one snapshot and one commit it says exactly what the
/// equal-ids branch above says without being asked: that this index describes
/// this repository. A worktree edit made after the verification is invisible to
/// it — and equally invisible to the equal-ids branch, which never walks at all.
/// This grants the moved-`HEAD` window the same trust as the unmoved one, which
/// is the whole claim of the commit that opened it.
fn index_still_holds(root: &Path, head: Option<rr_core::oid::Oid>) -> anyhow::Result<bool> {
    let verified = head.map_or_else(|| "none".to_owned(), |oid| oid.to_hex());
    if rr_core::workspace::read_memo(root, HEAD_VERIFIED_MEMO).as_deref() == Some(verified.as_str())
    {
        return Ok(true);
    }
    let report =
        rr_git::status(root, &CancelToken::new()).context("compare index to repository")?;
    // `Unknown` is ignorance, not evidence — a tree that could not be compared
    // has said nothing, and refusing on nothing is the behaviour that was wrong
    // in the first place. Named rather than excluded: `Missing`, `Corrupt` and
    // `Incompatible` are plans that would rebuild *everything*, which is more
    // than reconsidering paths, and a `!= Stale` test would read every one of
    // them as agreement.
    let holds = matches!(
        report.snapshot,
        SnapshotLabel::Fresh | SnapshotLabel::Unknown
    );
    // Only agreement is remembered. A refusal is the state a user is about to
    // change by running `rr refresh`, and a memo saying "stale" would have to be
    // invalidated by the very thing that fixes it.
    if holds {
        rr_core::workspace::write_memo(root, HEAD_VERIFIED_MEMO, &verified);
    }
    Ok(holds)
}

/// The memo naming the commit this snapshot was last confirmed to describe.
const HEAD_VERIFIED_MEMO: &str = "head-verified";

/// The cached answer to this question, if there is one that is still true.
///
/// Three things have to hold, and each of them is a way a route dies: the
/// record exists, its anchor still names exactly one symbol, and the repository's
/// public API still hashes to what the record was learned against. The third is
/// the one that matters, and it is deliberately corpus-wide rather than
/// per-scope. A per-scope key answers "is this symbol still what it was", which
/// is not the question: learn `verify token` → `src/auth/token.rs#verify_token`,
/// then add a better-matching `verify_token_request` under `src/api/`, and
/// `src/auth`'s own hash is untouched while the answer the ranker would now give
/// is not. Over-invalidating costs one ranked query; under-invalidating costs a
/// wrong answer.
///
/// Cheap in the order it asks, which is what makes a hit a shortcut rather than
/// a detour: the file read and the anchor resolution come first, and
/// [`api_identity`] is a memo read whenever a previous run already projected
/// this snapshot.
fn route_lookup(workspace: &Workspace, key: &RouteKey) -> Option<Candidate> {
    let (table, _) = load_routes(&workspace.root);
    let record = table.get(key)?;
    let symbol = resolve_route_anchor(&workspace.snapshot, &record.anchor)?;
    let identity = api_identity(&workspace.root, &workspace.snapshot, DEFAULT_MAP_BUDGET).ok()?;
    if identity != record.api_identity {
        return None;
    }
    Some(Candidate::new(
        TargetId::Symbol(symbol),
        Some(record.confidence),
    ))
}

/// Records the answer, if it is the kind of answer that can be recorded.
///
/// Best-effort in the strongest sense: this returns `()` and every failure
/// inside it is dropped. A query that answered correctly and then could not
/// write its cache has still answered correctly, and turning that into a
/// non-zero exit would make the cache a liability.
fn learn_route(workspace: &Workspace, key: Option<&RouteKey>, result: &QueryResult) {
    let Some(key) = key else { return };
    let QueryResult::Direct { candidate, .. } = result else {
        return;
    };
    // Only a symbol: a file target has no owning scope and so could never be
    // told it went stale.
    let TargetId::Symbol(symbol) = candidate.target else {
        return;
    };
    let Some(confidence) = candidate.confidence else {
        return;
    };
    // Projected, not validated: the weaker guarantee is that the map may not be
    // on disk, which costs a reader one `MAP.md` they cannot open, against
    // reading every artifact in the repository to learn one route.
    //
    // This is the one place on the query path that projects, and it is on the
    // miss — the run that already paid for a full ranking pass. `remember` files
    // the corpus identity it computed here, so the hits that follow read a memo
    // instead of projecting again.
    let Ok(catalog) = projected_map_catalog(&workspace.snapshot, DEFAULT_MAP_BUDGET) else {
        return;
    };
    catalog.remember(&workspace.root);
    let Some(owner) = catalog.owner(symbol) else {
        return;
    };
    let Ok(anchor) = resolve_anchor(&workspace.snapshot, candidate.target) else {
        return;
    };

    let record = RouteRecord {
        key: key.clone(),
        // The spelling `rr query` prints, so what is cached and what was shown
        // are one string.
        anchor: encode_anchor(anchor.path, anchor.symbol),
        map: encode_map_destination(owner.path().as_str()),
        api_identity: catalog.api_identity(),
        confidence,
    };
    let _ = update_routes(&workspace.root, |table| table.insert(record));
}

/// Verifies and attaches the anchor's source, in the one order that keeps
/// staleness ahead of format diagnosis and content behind the final check.
///
/// A result that is not a single direct anchor is left untouched, so asking for
/// source can never turn candidates or a no-match into filesystem work.
fn attach_source(workspace: &Workspace, result: &mut QueryResult) -> anyhow::Result<()> {
    let QueryResult::Direct {
        candidate, source, ..
    } = result
    else {
        return Ok(());
    };

    let indexed = resolve_indexed_source(&workspace.snapshot, candidate.target)
        .map_err(anyhow::Error::new)
        .context("resolve indexed source")?;
    let path = indexed.path();

    let acquired = acquire_for_source(workspace.repo.as_ref(), &workspace.root, path)
        .with_context(|| format!("acquire source for {}", path.as_str()))?;

    let pending = match verify_source(&indexed, acquired.as_source())
        .map_err(anyhow::Error::new)
        .with_context(|| format!("verify source for {}", path.as_str()))?
    {
        PendingSource::Refused(status) => {
            *source = Some(SourceResult::Refused { status });
            return Ok(());
        }
        PendingSource::Pending(pending) => pending,
    };

    // A pending packet only exists for an acquisition that carried content, so
    // this holds; failing closed is still cheaper than assuming it.
    let AcquireOutcome::Acquired(content) = &acquired else {
        bail!("verified source for {} without content", path.as_str());
    };

    // The packet exists but cannot be read yet: only a fresh final check
    // releases its content.
    let final_state = revalidate_source(workspace.repo.as_ref(), &workspace.root, path, content)
        .with_context(|| format!("revalidate source for {}", path.as_str()))?;

    *source = Some(finish_source(pending, final_state));
    Ok(())
}
