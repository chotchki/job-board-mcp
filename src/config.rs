//! Board configuration, loaded from TOML. The real watch list is private — this layer
//! ships only `config.example.toml`; the user's file lives wherever they keep private
//! things.
//!
//! Config is human-authored and single-reader — one binary reads its own file — so it
//! uses `deny_unknown_fields`: a typo'd key must FAIL LOUD rather than silently default
//! and mislead a scan. That's the opposite of the [`Posting`](crate::model::Posting)
//! choice, and deliberately so: Posting is cross-version machine data, config is not.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{Ats, AtsToken, BoardId};

/// Top-level config: where the store lives and which boards to track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Path to the SQLite store. Kept verbatim here (`~` and env are expanded when the
    /// store is opened, not at parse time).
    pub db_path: String,
    #[serde(default, rename = "board")]
    pub boards: Vec<BoardConfig>,
}

/// One configured board. Mirrors a `[[board]]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardConfig {
    /// Our name for the board (also its snapshot key).
    pub id: BoardId,
    pub ats: Ats,
    /// The ATS tenant slug in the board's API URL. For Workday this is the full API host
    /// instead (e.g. `"nvidia.wd5.myworkdayjobs.com"`), paired with [`site`](Self::site).
    pub token: AtsToken,
    /// Workday only: the career-site id (e.g. `"NVIDIAExternalCareerSite"`). Ignored by
    /// every other ATS.
    #[serde(default)]
    pub site: Option<String>,
    /// Bands publish only on the company's rendered site, never in this ATS's API, so
    /// comp arrives as `Comp::SiteOnly`. Forward-compat default: an older config without
    /// the key loads with it false.
    #[serde(default)]
    pub comp_site_only: bool,
    /// This board bulk-touches `updated_at` during reindexes, so treat it as noise. Same
    /// name and polarity as the flag on `Posting` — nothing negates it.
    #[serde(default)]
    pub updated_at_unreliable: bool,
}

/// Things that go wrong loading config.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("no config path: pass --config <path> or set JOB_BOARD_MCP_CONFIG")]
    NoPath,
    #[error("reading config {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing config {path}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

impl Config {
    /// Read and parse a config file.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::from_toml(&text).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source,
        })
    }

    /// Parse config from a TOML string. The seam the tests drive, so they never touch
    /// the filesystem.
    pub fn from_toml(text: &str) -> Result<Config, toml::de::Error> {
        toml::from_str(text)
    }
}

/// The single door for reading `JOB_BOARD_MCP_CONFIG` — env access stays centralized
/// rather than scattered through the codebase.
fn config_path_from_env() -> Option<PathBuf> {
    std::env::var_os("JOB_BOARD_MCP_CONFIG").map(PathBuf::from)
}

/// Resolve the config path: `--config <path>` (or `--config=<path>`) wins, else
/// `JOB_BOARD_MCP_CONFIG`, else [`ConfigError::NoPath`].
pub fn resolve_config_path<I>(args: I) -> Result<PathBuf, ConfigError>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--config" {
            if let Some(path) = args.next() {
                return Ok(PathBuf::from(path));
            }
        } else if let Some(path) = arg.strip_prefix("--config=") {
            return Ok(PathBuf::from(path));
        }
    }
    config_path_from_env().ok_or(ConfigError::NoPath)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_example_parses() {
        // include_str! makes config.example.toml a tested fixture — it can't rot without
        // failing the build.
        let cfg = Config::from_toml(include_str!("../config.example.toml")).unwrap();
        assert_eq!(cfg.db_path, "~/.local/share/job-board-mcp/store.sqlite");
        assert_eq!(cfg.boards.len(), 8);
        let stripe = &cfg.boards[0];
        assert_eq!(stripe.id, BoardId::new("stripe"));
        assert_eq!(stripe.ats, Ats::Greenhouse);
        assert!(stripe.comp_site_only);
        assert!(!stripe.updated_at_unreliable);
    }

    #[test]
    fn old_shape_board_defaults_the_flags() {
        // A leaner/older board with neither flag: both default to false, no failure.
        let cfg = Config::from_toml(
            r#"
            db_path = "/tmp/store.sqlite"
            [[board]]
            id = "figma"
            ats = "lever"
            token = "figma"
            "#,
        )
        .unwrap();
        let b = &cfg.boards[0];
        assert!(!b.comp_site_only);
        assert!(!b.updated_at_unreliable);
    }

    #[test]
    fn a_typoed_key_fails_loud() {
        // deny_unknown_fields earning its keep: `comp_site_onyl` must not silently
        // default comp_site_only to false.
        let err = Config::from_toml(
            r#"
            db_path = "/tmp/store.sqlite"
            [[board]]
            id = "figma"
            ats = "lever"
            token = "figma"
            comp_site_onyl = true
            "#,
        );
        assert!(err.is_err(), "a misspelled key must fail, not default");
    }

    #[test]
    fn unknown_ats_in_config_fails_loud() {
        // A board on an unimplemented ATS fails to parse rather than becoming a board
        // nothing can fetch.
        let err = Config::from_toml(
            r#"
            db_path = "/tmp/store.sqlite"
            [[board]]
            id = "acme"
            ats = "jobvite"
            token = "acme"
            "#,
        );
        assert!(err.is_err());
    }

    #[test]
    fn resolve_path_prefers_cli_over_env() {
        let args = ["--config".to_owned(), "/etc/jb.toml".to_owned()];
        assert_eq!(
            resolve_config_path(args).unwrap(),
            PathBuf::from("/etc/jb.toml")
        );
        let joined = ["--config=/etc/jb2.toml".to_owned()];
        assert_eq!(
            resolve_config_path(joined).unwrap(),
            PathBuf::from("/etc/jb2.toml")
        );
    }
}
