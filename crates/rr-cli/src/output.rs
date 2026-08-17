use std::io::{self, Write};

/// Formats and emits all CLI user-facing output.
pub struct Output;

impl Output {
    /// Formats and writes the version and git SHA to the given writer.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if writing fails.
    pub fn write_version<W: Write>(mut writer: W, version: &str, git_sha: &str) -> io::Result<()> {
        writeln!(writer, "rr {version} ({git_sha})")
    }

    /// Prints the binary version and compiled commit SHA to stdout.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if writing to `stdout` fails.
    pub fn print_version(version: &str, git_sha: &str) -> io::Result<()> {
        let stdout = io::stdout().lock();
        Self::write_version(stdout, version, git_sha)
    }

    /// Writes one line per recognised language: name, tier, extractor version.
    ///
    /// Every language, not only the compiled ones. A user whose repository is
    /// half Kotlin needs to read that rr saw it and could only say identifiers
    /// about it; a table that listed the languages this build parses would
    /// answer a question they did not ask.
    ///
    /// Read from `Registry::indexable` — the list the walk allowlist itself
    /// comes from — rather than from a second filter over `Lang::ALL`. A table
    /// that derived the set on its own could advertise a language the walk had
    /// stopped visiting, and a user comparing the two would have no way to tell
    /// which one was lying.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if writing fails.
    pub fn write_languages<W: Write>(mut writer: W) -> io::Result<()> {
        for lang in rr_core::parser::Registry::indexable() {
            let tier = rr_core::tier(lang);
            let version = rr_core::extractor_version(lang);
            writeln!(
                writer,
                "{:<12} {:<9} {version}",
                lang.as_str(),
                tier.as_str()
            )?;
        }
        Ok(())
    }

    /// Prints the language table to stdout.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if writing to `stdout` fails.
    pub fn print_languages() -> io::Result<()> {
        Self::write_languages(io::stdout().lock())
    }

    /// Formats and writes a plain text line to the given writer.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if writing fails.
    pub fn write_text<W: Write>(mut writer: W, text: &str) -> io::Result<()> {
        writeln!(writer, "{text}")
    }

    /// Prints a plain text line to standard output.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if writing to `stdout` fails.
    pub fn print_text(text: &str) -> io::Result<()> {
        let stdout = io::stdout().lock();
        Self::write_text(stdout, text)
    }

    /// Prints a diagnostic to standard error.
    ///
    /// Separate from [`Self::print_text`] so a report a script is parsing on
    /// stdout is never interleaved with the reason a run refused.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if writing to `stderr` fails.
    pub fn print_error(text: &str) -> io::Result<()> {
        let stderr = io::stderr().lock();
        Self::write_text(stderr, text)
    }

    /// Writes raw output bytes to standard output without adding extra newlines.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if writing or flushing fails.
    pub fn print_raw(content: &str) -> io::Result<()> {
        let mut stdout = io::stdout().lock();
        stdout.write_all(content.as_bytes())?;
        stdout.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::Output;

    #[test]
    fn test_write_version() {
        let mut buf = Vec::new();
        Output::write_version(&mut buf, "0.1.0", "545560c").unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "rr 0.1.0 (545560c)\n");
    }

    #[test]
    fn test_write_text() {
        let mut buf = Vec::new();
        Output::write_text(&mut buf, "hello world").unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "hello world\n");
    }
}
