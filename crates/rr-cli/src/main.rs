mod output;

use clap::{Parser, Subcommand};
use output::Output;

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
    /// Display version and build information
    Version,
}

fn main() -> anyhow::Result<()> {
    #[cfg(unix)]
    // Reset SIGPIPE handler to default behavior so pipeline terminations (e.g. `rr ... | head`) exit cleanly.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let cli = Cli::parse();

    match cli.command {
        Commands::Version => {
            let version = env!("CARGO_PKG_VERSION");
            let git_sha = env!("RR_GIT_HASH");
            Output::print_version(version, git_sha)?;
        }
    }

    Ok(())
}
