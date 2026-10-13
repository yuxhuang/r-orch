use std::path::PathBuf;
use std::sync::Arc;

use orch_core::process::ProcessManager;
use orch_core::workspace::MergeResult;

use super::run;

/// `git merge --no-ff`, aborting on conflict. The one and only merge strategy.
pub struct MergeStrategy {
    project_root: PathBuf,
    process_manager: Arc<dyn ProcessManager>,
}

/// `conflictInfo` is `output.slice(0, 1000)` (UTF-16 units).
const CONFLICT_INFO_LEN: usize = 1000;

fn truncate_utf16(s: &str, max_units: usize) -> String {
    let mut units = 0;
    s.chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= max_units
        })
        .collect()
}

impl MergeStrategy {
    pub fn new(project_root: PathBuf, process_manager: Arc<dyn ProcessManager>) -> Self {
        Self {
            project_root,
            process_manager,
        }
    }

    /// Never fails: every problem is a [`MergeResult::Conflict`] carrying the
    /// (truncated) git output or the spawn error message.
    pub async fn merge_back(&self, branch: &str) -> MergeResult {
        let args = [
            "merge".to_owned(),
            "--no-ff".to_owned(),
            branch.to_owned(),
            "-m".to_owned(),
            format!("Merge {branch}"),
        ];
        let merged = match run(&*self.process_manager, &self.project_root, "git", &args).await {
            Ok(captured) => captured,
            Err(message) => {
                tracing::warn!(branch, %message, "git merge could not be spawned");
                return MergeResult::Conflict {
                    conflict_info: message,
                };
            }
        };
        if merged.code == 0 {
            return MergeResult::Success;
        }

        let info = truncate_utf16(&merged.combined, CONFLICT_INFO_LEN);
        let is_conflict = info.contains("CONFLICT") || info.contains("Merge conflict");
        if !is_conflict {
            tracing::warn!(
                branch,
                code = merged.code,
                "git merge failed without a conflict"
            );
            return MergeResult::Conflict {
                conflict_info: info,
            };
        }

        tracing::warn!(branch, "merge conflict; aborting the merge");
        let abort = ["merge".to_owned(), "--abort".to_owned()];
        if let Err(message) = run(&*self.process_manager, &self.project_root, "git", &abort).await {
            tracing::warn!(branch, %message, "git merge --abort could not be spawned");
        }
        MergeResult::Conflict {
            conflict_info: info,
        }
    }
}
