//! Global config store.
//!
//! `~/.orchestry/global.yml`, shared across projects, with the path
//! overridable by `ORCHESTRY_GLOBAL_CONFIG_PATH` (trimmed and resolved).
//! Reads are **defensive**: an unknown palette falls back to `amber`,
//! `activity_filter` is deliberately *not* validated, and a non-boolean
//! notification flag falls back to its default
//! ([`GlobalConfig::from_raw`]).
//!
//! The override is the only thing that keeps tests off the developer's real
//! preferences, so it is read per call rather than
//! cached.

use std::path::PathBuf;

use async_trait::async_trait;
use orch_core::error::{Error, Result};
use orch_core::storage::GlobalConfigStore;
use orch_domain::{GlobalConfig, default_global_config};

use super::fs_utils::{ensure_dir, read_yaml_value, write_yaml};

/// `ORCHESTRY_GLOBAL_CONFIG_PATH` redirects `global.yml` **only** — the two
/// update-check files have no override at all, which is why tests that
/// isolate it set `HOME` as well.
pub const GLOBAL_CONFIG_PATH_ENV: &str = "ORCHESTRY_GLOBAL_CONFIG_PATH";

#[derive(Debug, Clone, Default)]
pub struct FileGlobalConfigStore {
    /// An explicit path, bypassing both the environment and the home
    /// directory. Tests use it; production passes `None`.
    override_path: Option<PathBuf>,
}

impl FileGlobalConfigStore {
    pub fn new() -> Self {
        FileGlobalConfigStore::default()
    }

    pub fn at(path: impl Into<PathBuf>) -> Self {
        FileGlobalConfigStore {
            override_path: Some(path.into()),
        }
    }

    /// The resolved config path: the explicit override, else
    /// `ORCHESTRY_GLOBAL_CONFIG_PATH` (trimmed, made absolute), else
    /// `~/.orchestry/global.yml`.
    pub fn path(&self) -> PathBuf {
        resolve_path(
            self.override_path.as_deref(),
            std::env::var_os(GLOBAL_CONFIG_PATH_ENV)
                .map(|raw| raw.to_string_lossy().into_owned())
                .as_deref(),
            dirs::home_dir().as_deref(),
            std::env::current_dir().ok().as_deref(),
        )
    }
}

/// Path resolution as a pure function of its three sources.
///
/// Pulled out of [`FileGlobalConfigStore::path`] so the precedence can be
/// tested without mutating the process environment, which parallel tests
/// share.
fn resolve_path(
    override_path: Option<&std::path::Path>,
    env_value: Option<&str>,
    home: Option<&std::path::Path>,
    cwd: Option<&std::path::Path>,
) -> PathBuf {
    if let Some(path) = override_path {
        return path.to_path_buf();
    }
    if let Some(trimmed) = env_value.map(str::trim).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(trimmed);
        return if path.is_absolute() {
            path
        } else {
            // `path.resolve` is relative to the working directory.
            cwd.unwrap_or(std::path::Path::new("")).join(path)
        };
    }
    // An absent home is pathological; fall back to a relative path rather than
    // panicking, so a read still yields the defaults.
    home.unwrap_or(std::path::Path::new(""))
        .join(".orchestry/global.yml")
}

#[async_trait]
impl GlobalConfigStore for FileGlobalConfigStore {
    async fn read(&self) -> Result<GlobalConfig> {
        let path = self.path();
        // A malformed file must not break every command that reads
        // preferences, so a parse failure degrades to the defaults — a missing
        // file is treated the same way and nothing downstream can act on the
        // error.
        match read_yaml_value(&path).await {
            Ok(Some(value)) => {
                let json = serde_json::to_value(&value).map_err(|err| {
                    Error::serde(format!("convert {}", path.display()), &path, err)
                })?;
                Ok(GlobalConfig::from_raw(Some(&json)))
            }
            Ok(None) => Ok(default_global_config()),
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "unreadable global config, using defaults"
                );
                Ok(default_global_config())
            }
        }
    }

    async fn write(&self, config: &GlobalConfig) -> Result<()> {
        let path = self.path();
        if let Some(parent) = path.parent() {
            ensure_dir(parent).await?;
        }
        write_yaml(&path, config).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::fs_utils::{atomic_write, clear_ensured_dirs};
    use orch_domain::TuiPaletteName;

    fn store(root: &std::path::Path) -> FileGlobalConfigStore {
        clear_ensured_dirs();
        FileGlobalConfigStore::at(root.join(".orchestry/global.yml"))
    }

    #[tokio::test]
    async fn a_missing_file_reads_as_the_defaults() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(
            store(temp.path()).read().await.unwrap(),
            default_global_config()
        );
    }

    #[tokio::test]
    async fn write_creates_the_parent_directory_and_round_trips() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let mut config = default_global_config();
        config.tui.palette = TuiPaletteName::Ocean;
        config.tui.activity_filter = "errors".into();
        config.tui.notifications.bell = true;

        store.write(&config).await.unwrap();
        assert_eq!(store.read().await.unwrap(), config);
    }

    #[tokio::test]
    async fn the_written_file_has_the_expected_shape() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        store.write(&default_global_config()).await.unwrap();

        assert_eq!(
            tokio::fs::read_to_string(store.path()).await.unwrap(),
            "tui:\n  palette: amber\n  activity_filter: all\n  notifications:\n    toast: true\n    bell: false\n"
        );
    }

    #[tokio::test]
    async fn an_unknown_palette_falls_back_to_amber() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        clear_ensured_dirs();
        tokio::fs::create_dir_all(store.path().parent().unwrap())
            .await
            .unwrap();
        atomic_write(&store.path(), "tui:\n  palette: chartreuse\n")
            .await
            .unwrap();

        assert_eq!(
            store.read().await.unwrap().tui.palette,
            TuiPaletteName::Amber
        );
    }

    #[tokio::test]
    async fn activity_filter_is_not_validated() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        clear_ensured_dirs();
        tokio::fs::create_dir_all(store.path().parent().unwrap())
            .await
            .unwrap();
        atomic_write(&store.path(), "tui:\n  activity_filter: something-new\n")
            .await
            .unwrap();

        // Deliberately passes through.
        assert_eq!(
            store.read().await.unwrap().tui.activity_filter,
            "something-new"
        );
    }

    #[tokio::test]
    async fn non_boolean_notification_flags_fall_back() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        clear_ensured_dirs();
        tokio::fs::create_dir_all(store.path().parent().unwrap())
            .await
            .unwrap();
        atomic_write(
            &store.path(),
            "tui:\n  notifications:\n    toast: \"yes\"\n    bell: 1\n",
        )
        .await
        .unwrap();

        let config = store.read().await.unwrap();
        assert!(config.tui.notifications.toast);
        assert!(!config.tui.notifications.bell);
    }

    #[tokio::test]
    async fn a_malformed_file_degrades_to_the_defaults_rather_than_failing() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        clear_ensured_dirs();
        tokio::fs::create_dir_all(store.path().parent().unwrap())
            .await
            .unwrap();
        atomic_write(&store.path(), "tui: [this is: not valid: yaml\n")
            .await
            .unwrap();

        // Nothing downstream can act on the error, and every command that
        // reads preferences would otherwise break.
        assert_eq!(store.read().await.unwrap(), default_global_config());
    }

    #[tokio::test]
    async fn an_empty_file_reads_as_the_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        clear_ensured_dirs();
        tokio::fs::create_dir_all(store.path().parent().unwrap())
            .await
            .unwrap();
        atomic_write(&store.path(), "").await.unwrap();
        assert_eq!(store.read().await.unwrap(), default_global_config());
    }

    #[test]
    fn path_precedence_is_override_then_environment_then_home() {
        use std::path::Path;
        let explicit = Path::new("/tmp/explicit.yml");
        let home = Path::new("/home/someone");
        let cwd = Path::new("/work");

        // The explicit override wins over everything.
        assert_eq!(
            resolve_path(
                Some(explicit),
                Some("/tmp/from-env.yml"),
                Some(home),
                Some(cwd)
            ),
            explicit
        );
        // Then the environment, absolute as given.
        assert_eq!(
            resolve_path(None, Some("/tmp/from-env.yml"), Some(home), Some(cwd)),
            Path::new("/tmp/from-env.yml")
        );
        // A relative override resolves against the working directory.
        assert_eq!(
            resolve_path(None, Some("rel/global.yml"), Some(home), Some(cwd)),
            Path::new("/work/rel/global.yml")
        );
        // Surrounding whitespace is trimmed.
        assert_eq!(
            resolve_path(None, Some("  /tmp/padded.yml \t"), Some(home), Some(cwd)),
            Path::new("/tmp/padded.yml")
        );
        // An empty or whitespace-only value is ignored.
        for env in ["", "   "] {
            assert_eq!(
                resolve_path(None, Some(env), Some(home), Some(cwd)),
                Path::new("/home/someone/.orchestry/global.yml"),
                "{env:?}"
            );
        }
        // Finally the home directory.
        assert_eq!(
            resolve_path(None, None, Some(home), Some(cwd)),
            Path::new("/home/someone/.orchestry/global.yml")
        );
        // An absent home degrades to a relative path rather than panicking.
        assert_eq!(
            resolve_path(None, None, None, Some(cwd)),
            Path::new(".orchestry/global.yml")
        );
    }

    #[test]
    fn the_real_path_ends_at_the_expected_file_name() {
        // Whatever the ambient environment says, the file name is fixed.
        let path = FileGlobalConfigStore::new().path();
        assert_eq!(path.file_name().unwrap(), "global.yml");
    }
}
