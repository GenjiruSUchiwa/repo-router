use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Error returned when parsing or creating an invalid [`RelPath`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RelPathError {
    #[error("relative path cannot be empty")]
    Empty,
    #[error("path is not relative to root: {0}")]
    NotRelative(String),
    #[error("path contains invalid component (e.g. '..'): {0}")]
    InvalidComponent(String),
}

/// A normalized, repo-relative path using `/` as separator.
///
/// Invariants:
/// - Never empty.
/// - Never starts with `/` or `./`.
/// - Never contains `..` components.
/// - Uses `/` (forward slash) on all platforms for snapshot determinism.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RelPath(String);

impl RelPath {
    /// Creates a new [`RelPath`] from a string slice, normalizing separators.
    ///
    /// # Errors
    /// Returns [`RelPathError::Empty`] if the path is empty, or [`RelPathError::InvalidComponent`]
    /// if the path contains illegal segments such as `..`.
    pub fn new(path: impl AsRef<str>) -> Result<Self, RelPathError> {
        let raw = path.as_ref();
        Self::normalize_str(raw)
    }

    /// Creates a new [`RelPath`] by stripping `root` from `full_path`.
    ///
    /// # Errors
    /// Returns [`RelPathError::NotRelative`] if `full_path` is not inside `root`,
    /// or [`RelPathError::Empty`] / [`RelPathError::InvalidComponent`] on invalid components.
    pub fn from_path(root: &Path, full_path: &Path) -> Result<Self, RelPathError> {
        let relative = match full_path.strip_prefix(root) {
            Ok(rel) => rel,
            Err(_) => {
                if full_path.is_relative() {
                    full_path
                } else {
                    return Err(RelPathError::NotRelative(full_path.display().to_string()));
                }
            }
        };
        Self::from_relative_path(relative)
    }

    /// Creates a new [`RelPath`] from a relative [`Path`].
    ///
    /// # Errors
    /// Returns [`RelPathError::Empty`] if the path is empty, or [`RelPathError::InvalidComponent`]
    /// if the path contains non-normal components such as `..` or root prefixes.
    pub fn from_relative_path(path: &Path) -> Result<Self, RelPathError> {
        if path.as_os_str().is_empty() {
            return Err(RelPathError::Empty);
        }

        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(c) => {
                    let s = c.to_str().ok_or_else(|| {
                        RelPathError::InvalidComponent(path.display().to_string())
                    })?;
                    parts.push(s);
                }
                Component::CurDir => {
                    // ignore '.'
                }
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(RelPathError::InvalidComponent(path.display().to_string()));
                }
            }
        }

        if parts.is_empty() {
            return Err(RelPathError::Empty);
        }

        Ok(Self(parts.join("/")))
    }

    /// Normalizes a string representation into a valid [`RelPath`].
    fn normalize_str(raw: &str) -> Result<Self, RelPathError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(RelPathError::Empty);
        }

        // Split by either '/' or '\'
        let mut parts = Vec::new();
        for segment in trimmed.split(['/', '\\']) {
            let seg = segment.trim();
            if seg.is_empty() || seg == "." {
                continue;
            }
            if seg == ".." {
                return Err(RelPathError::InvalidComponent(raw.to_string()));
            }
            parts.push(seg);
        }

        if parts.is_empty() {
            return Err(RelPathError::Empty);
        }

        Ok(Self(parts.join("/")))
    }

    /// Returns the normalized relative path as a string slice.
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the path as a standard [`Path`].
    #[inline]
    #[must_use]
    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }

    /// Converts to a standard [`PathBuf`].
    #[inline]
    #[must_use]
    pub fn to_path_buf(&self) -> PathBuf {
        PathBuf::from(&self.0)
    }

    /// Returns the file name component, if any.
    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        self.0.rsplit('/').next()
    }

    /// Returns the file extension, if any.
    #[must_use]
    pub fn extension(&self) -> Option<&str> {
        let name = self.file_name()?;
        let ext = name.rsplit('.').next()?;
        if ext == name {
            None
        } else {
            Some(ext)
        }
    }

    /// Returns the parent directory as a [`RelPath`], or `None` if at root.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        let idx = self.0.rfind('/')?;
        Some(Self(self.0[..idx].to_string()))
    }
}

impl Deref for RelPath {
    type Target = str;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<str> for RelPath {
    #[inline]
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl AsRef<Path> for RelPath {
    #[inline]
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl Borrow<str> for RelPath {
    #[inline]
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl Borrow<Path> for RelPath {
    #[inline]
    fn borrow(&self) -> &Path {
        self.as_path()
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Debug for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RelPath({:?})", self.0)
    }
}

impl FromStr for RelPath {
    type Err = RelPathError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<&str> for RelPath {
    type Error = RelPathError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl TryFrom<String> for RelPath {
    type Error = RelPathError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl TryFrom<&Path> for RelPath {
    type Error = RelPathError;

    fn try_from(path: &Path) -> Result<Self, Self::Error> {
        Self::from_relative_path(path)
    }
}

impl TryFrom<PathBuf> for RelPath {
    type Error = RelPathError;

    fn try_from(path: PathBuf) -> Result<Self, Self::Error> {
        Self::from_relative_path(&path)
    }
}

impl From<RelPath> for PathBuf {
    fn from(rel: RelPath) -> Self {
        PathBuf::from(rel.0)
    }
}

impl From<&RelPath> for PathBuf {
    fn from(rel: &RelPath) -> Self {
        PathBuf::from(&rel.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_rel_paths() {
        let p = RelPath::new("src/auth/token.rs").unwrap();
        assert_eq!(p.as_str(), "src/auth/token.rs");
        assert_eq!(p.file_name(), Some("token.rs"));
        assert_eq!(p.extension(), Some("rs"));
        assert_eq!(p.parent().unwrap().as_str(), "src/auth");

        let p2 = RelPath::new("./src/auth/token.rs").unwrap();
        assert_eq!(p2.as_str(), "src/auth/token.rs");

        let p3 = RelPath::new("src\\auth\\token.rs").unwrap();
        assert_eq!(p3.as_str(), "src/auth/token.rs");
    }

    #[test]
    fn test_invalid_rel_paths() {
        assert_eq!(RelPath::new("").unwrap_err(), RelPathError::Empty);
        assert_eq!(RelPath::new("   ").unwrap_err(), RelPathError::Empty);
        assert_eq!(
            RelPath::new("../foo.rs").unwrap_err(),
            RelPathError::InvalidComponent("../foo.rs".to_string())
        );
        assert_eq!(
            RelPath::new("src/../../foo.rs").unwrap_err(),
            RelPathError::InvalidComponent("src/../../foo.rs".to_string())
        );
    }

    #[test]
    fn test_from_path() {
        let root = Path::new("/repo/root");
        let full = Path::new("/repo/root/src/main.rs");
        let rel = RelPath::from_path(root, full).unwrap();
        assert_eq!(rel.as_str(), "src/main.rs");
    }

    #[test]
    fn test_extension() {
        assert_eq!(RelPath::new("file.rs").unwrap().extension(), Some("rs"));
        assert_eq!(RelPath::new("Dockerfile").unwrap().extension(), None);
        assert_eq!(
            RelPath::new("archive.tar.gz").unwrap().extension(),
            Some("gz")
        );
    }
}
