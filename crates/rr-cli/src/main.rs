mod output;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use output::Output;
use rr_core::path::RelPath;
use rr_core::query::{parse_query, route_query, QueryRequest};
use rr_core::ranking::{RankingScratch, DEFAULT_RANKING_PROFILE};
use rr_core::render::{render_json, render_json_explained, render_text, render_text_explained};
use rr_core::snapshot::{LoadOutcome, SnapshotStore};
use rr_git::{build_map, GitRepo};

const VERSION_STR: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("RR_GIT_HASH"), ")");

#[derive(Parser, Debug)]
#[command(
    name = "rr",
    version = VERSION_STR,
    about = "Repository router for AI coding agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Version,
    Map {
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long)]
        threads: Option<usize>,
        #[arg(long, short)]
        verbose: bool,
    },
    Query {
        #[arg(long, value_name = "REL_PATH")]
        path: Option<RelPath>,
        #[arg(long)]
        json: bool,
        /// Report the work the ranker did, including whether the candidate cap
        /// discarded members it could not tell apart from the ones it kept.
        #[arg(long)]
        explain: bool,
        query: String,
    },
}

fn main() -> ExitCode {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let cli = Cli::parse();
    match cli.command {
        Commands::Version => {
            if let Err(err) = Output::print_version(env!("CARGO_PKG_VERSION"), env!("RR_GIT_HASH"))
            {
                eprintln!("rr: {err}");
                return ExitCode::from(1);
            }
            ExitCode::from(0)
        }
        Commands::Map {
            root,
            threads,
            verbose,
        } => match run_map(root, threads, verbose) {
            Ok(()) => ExitCode::from(0),
            Err(err) => {
                eprintln!("rr: {err:#}");
                ExitCode::from(1)
            }
        },
        Commands::Query {
            path,
            json,
            explain,
            query,
        } => match run_query(path.as_ref(), json, explain, &query) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("rr: query: {err:#}");
                // Exit 1 for all errors: SPEC reserves 2 for the candidates
                // outcome, so errors must not collide with it.
                ExitCode::from(1)
            }
        },
    }
}

fn run_map(root: Option<PathBuf>, threads: Option<usize>, verbose: bool) -> anyhow::Result<()> {
    let root = root.unwrap_or(std::env::current_dir().context("resolve current directory")?);
    let thread_count = threads.unwrap_or(1);
    if thread_count == 0 {
        bail!("--threads must be greater than zero");
    }
    let started = Instant::now();
    let report = build_map(&root, thread_count).context("build repository map")?;
    let canonical = root.canonicalize().context("canonicalize map root")?;
    let store_root = GitRepo::discover(&canonical)
        .context("discover repository for snapshot path")?
        .map_or(canonical, |repo| repo.workdir().to_path_buf());
    SnapshotStore::new(&store_root)
        .write(&report.snapshot)
        .context("publish snapshot")?;

    let total_cache =
        report.stats.cache_hits + report.stats.cache_misses + report.stats.cache_corrupt;
    let cache_rate = report
        .stats
        .cache_hits
        .saturating_mul(100)
        .checked_div(total_cache)
        .unwrap_or_default();
    let line = format!(
        "rr: mapped {} files ({} symbols, {} refs) in {:.2}s (cache: {}% hits)",
        report.stats.files,
        report.stats.symbols,
        report.stats.references,
        started.elapsed().as_secs_f64(),
        cache_rate
    );
    Output::print_text(&line)?;
    if verbose {
        let stats = format!(
            "  workers: {} clean probes, {} parses ({} complete, {} recovered, {} degraded)\n  cache: {} hits, {} misses, {} corrupt\n  references: {} unresolved, {} ambiguous\n  imports: {} unresolved, {} ambiguous",
            report.stats.clean_probes,
            report.stats.parses,
            report.stats.complete,
            report.stats.recovered,
            report.stats.degraded,
            report.stats.cache_hits,
            report.stats.cache_misses,
            report.stats.cache_corrupt,
            report.stats.unresolved_refs,
            report.stats.ambiguous_refs,
            report.stats.unresolved_imports,
            report.stats.ambiguous_imports
        );
        Output::print_text(&stats)?;
    }
    Ok(())
}

fn run_query(
    path: Option<&RelPath>,
    json: bool,
    explain: bool,
    query_str: &str,
) -> anyhow::Result<u8> {
    let current_dir = std::env::current_dir().context("resolve current directory")?;
    let canonical = current_dir
        .canonicalize()
        .context("canonicalize current directory")?;
    let git_repo = GitRepo::discover(&canonical).context("discover repository")?;

    let (store_root, is_git, head_oid) = match &git_repo {
        Some(repo) => {
            let head = repo.head_oid().context("resolve HEAD commit")?;
            (repo.workdir().to_path_buf(), true, head)
        }
        None => (canonical, false, None),
    };

    let store = SnapshotStore::new(&store_root);
    let outcome = store.load().map_err(|err| anyhow::anyhow!("{err}"))?;

    let snapshot = match outcome {
        LoadOutcome::Ready(snap) => snap,
        LoadOutcome::Missing => {
            bail!("index missing; run 'rr map'");
        }
        LoadOutcome::NeedsRebuild(_) => {
            bail!("index invalid; run 'rr map'");
        }
    };

    if is_git {
        if snapshot.meta.no_git {
            bail!("index repository mismatch; run 'rr map'");
        }
        if snapshot.meta.repo_head_oid != head_oid {
            bail!("index is stale; run 'rr refresh'");
        }
    } else if !snapshot.meta.no_git {
        bail!("index repository mismatch; run 'rr map'");
    }

    let request = QueryRequest::new(query_str, path);
    let parsed = parse_query(&snapshot, request).map_err(anyhow::Error::new)?;
    let mut scratch = RankingScratch::new();
    let (result, evidence) =
        route_query(&snapshot, &parsed, &DEFAULT_RANKING_PROFILE, &mut scratch)
            .map_err(anyhow::Error::new)?;

    let rendered = match (json, explain) {
        (true, false) => render_json(&snapshot, &result),
        (true, true) => render_json_explained(&snapshot, &result, evidence.as_ref()),
        (false, false) => render_text(&snapshot, &result),
        (false, true) => render_text_explained(&snapshot, &result, evidence.as_ref()),
    }
    .map_err(|err| anyhow::anyhow!("{err}"))?;

    Output::print_raw(&rendered).context("write query result")?;
    Ok(result.exit_code())
}
