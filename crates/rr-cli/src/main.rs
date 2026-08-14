mod output;
mod query;

use std::path::PathBuf;
use std::process::ExitCode;
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
        #[arg(long, short)]
        verbose: bool,
    },
    Query(query::Args),
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
                eprintln!("rr: {}", one_line(&err));
                ExitCode::from(1)
            }
        },
        Commands::Query(args) => match query::run(&args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("rr: query: {}", one_line(&err));
                // Exit 1 for all errors: SPEC reserves 2 for the candidates
                // outcome, so errors must not collide with it.
                ExitCode::from(1)
            }
        },
    }
}

/// Flattens an error chain into a single diagnostic line.
///
/// A failure deep in the stack can carry text we did not author — an external
/// filter's stderr, an OS message in another locale — and a newline in it would
/// turn one diagnostic into two, which is exactly the shape a caller reading
/// stderr line by line would misread as a second failure.
fn one_line(err: &anyhow::Error) -> String {
    let rendered = format!("{err:#}");
    let mut line = String::with_capacity(rendered.len());
    let mut pending_space = false;
    for character in rendered.chars() {
        // U+2028/U+2029 are not control characters but still break a line for
        // any reader that splits on Unicode line boundaries rather than on LF.
        if character.is_control() || matches!(character, '\u{2028}' | '\u{2029}') {
            pending_space = !line.is_empty();
            continue;
        }
        if pending_space {
            // The whitespace that followed a break was indentation for a line
            // that no longer exists, so it joins the break rather than the text.
            if character.is_whitespace() {
                continue;
            }
            line.push(' ');
            pending_space = false;
        }
        line.push(character);
    }
    line
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

#[cfg(test)]
mod tests {
    use super::one_line;

    #[test]
    fn a_multi_line_failure_still_reports_as_one_diagnostic() {
        let inner = anyhow::anyhow!("filter said:\n  bad magic\r\n  at offset 3\u{2028}stop");
        let err = inner.context("acquire source for src/lib.rs");

        let rendered = one_line(&err);

        assert_eq!(rendered.lines().count(), 1);
        assert_eq!(
            rendered,
            "acquire source for src/lib.rs: filter said: bad magic at offset 3 stop"
        );
    }

    #[test]
    fn an_ordinary_failure_survives_unchanged() {
        let err = anyhow::anyhow!("no such file: src/lib.rs");

        assert_eq!(one_line(&err), "no such file: src/lib.rs");
    }
}
