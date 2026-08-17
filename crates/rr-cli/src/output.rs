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
