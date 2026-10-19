//! Config errors that point at the offending file, line and column.

use std::{fmt, path::PathBuf};

/// Where a config value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A config file (or in-memory text when the path is empty).
    File(PathBuf),
    /// An environment variable override.
    Env(String),
}

/// A config problem with its location and, when we can guess one, a fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// Source of the bad value.
    pub origin: Origin,
    /// 1-based line and column, for file origins.
    pub position: Option<(usize, usize)>,
    /// What is wrong.
    pub message: String,
    /// How to fix it.
    pub hint: Option<String>,
}

impl ConfigError {
    pub(crate) fn new(origin: Origin, message: impl Into<String>) -> Self {
        Self {
            origin,
            position: None,
            message: message.into(),
            hint: None,
        }
    }

    pub(crate) fn at(mut self, src: &str, offset: Option<usize>) -> Self {
        self.position = offset.map(|o| line_col(src, o));
        self
    }

    pub(crate) fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.origin {
            Origin::File(p) if p.as_os_str().is_empty() => f.write_str("<config>")?,
            Origin::File(p) => write!(f, "{}", p.display())?,
            Origin::Env(var) => write!(f, "environment variable {var}")?,
        }
        if let Some((line, col)) = self.position {
            write!(f, ":{line}:{col}")?;
        }
        write!(f, ": {}", self.message)?;
        if let Some(h) = &self.hint {
            write!(f, "\n  hint: {h}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigError {}

impl From<ConfigError> for mcpgw_core::Error {
    fn from(e: ConfigError) -> Self {
        mcpgw_core::Error::Config(e.to_string())
    }
}

/// 1-based line and column (in characters) of a byte offset.
pub(crate) fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(src.len());
    let before = &src[..src.floor_char_boundary(offset)];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    (line, col)
}

/// The candidate closest to `word` by edit distance, if close enough to be a
/// plausible typo.
pub(crate) fn did_you_mean<'a>(word: &str, candidates: &[&'a str]) -> Option<&'a str> {
    candidates
        .iter()
        .map(|c| (levenshtein(word, c), *c))
        .filter(|(d, c)| *d <= 2.max(c.len() / 3))
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != *cb);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_and_suggestions() {
        assert_eq!(line_col("ab\ncdé\nx", 0), (1, 1));
        assert_eq!(line_col("ab\ncdé\nx", 3), (2, 1));
        assert_eq!(line_col("ab\ncdé\nx", 8), (3, 1));
        assert_eq!(
            did_you_mean("comand", &["command", "args"]),
            Some("command")
        );
        assert_eq!(did_you_mean("zzzzzz", &["command", "args"]), None);
    }

    #[test]
    fn display() {
        let e = ConfigError::new(Origin::File("c.toml".into()), "bad")
            .at("a\nb", Some(2))
            .hint("fix it");
        assert_eq!(e.to_string(), "c.toml:2:1: bad\n  hint: fix it");
        let e = ConfigError::new(Origin::Env("MCPGW_X".into()), "bad");
        assert_eq!(e.to_string(), "environment variable MCPGW_X: bad");
    }
}
