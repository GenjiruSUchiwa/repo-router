mod output;

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use output::Output;
use rr_core::snapshot::SnapshotStore;
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
        #[arg(long)]
        verbose: bool,
    },
}

fn main() -> anyhow::Result<()> {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    match Cli::parse().command {
        Commands::Version => {
            Output::print_version(env!("CARGO_PKG_VERSION"), env!("RR_GIT_HASH"))?;
        }
        Commands::Map {
            root,
            threads,
            verbose,
        } => run_map(root, threads, verbose)?,
    }
    Ok(())
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
        "rr map — {} files, {} symbols, {} unresolved refs, {} ambiguous refs, {} ms (cache {}%; {} hits, {} misses, {} corrupt; {} reparsed)",
        report.stats.files,
        report.stats.symbols,
        report.stats.unresolved_refs,
        report.stats.ambiguous_refs,
        started.elapsed().as_millis(),
        cache_rate,
        report.stats.cache_hits,
        report.stats.cache_misses,
        report.stats.cache_corrupt,
        report.stats.reparsed,
    );
    Output::print_text(&line)?;
    if verbose {
        let details = format!(
            "imports {} ({} unresolved, {} ambiguous), references {}, clean probes {}, clean blob reads {}, filtered/raw reads {}, parses {} (complete {}, recovered {}, degraded {}), content reads {}, cache write failures {}",
            report.stats.imports,
            report.stats.unresolved_imports,
            report.stats.ambiguous_imports,
            report.stats.references,
            report.stats.clean_probes,
            report.stats.clean_blob_reads,
            report.stats.filtered_raw_reads,
            report.stats.parses,
            report.stats.complete,
            report.stats.recovered,
            report.stats.degraded,
            report.stats.content_reads,
            report.stats.cache_write_failures,
        );
        Output::print_text(&details)?;
    }
    Ok(())
}
