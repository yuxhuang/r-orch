//! `state.json` and the runs it says are at work (R119).
use super::PreflightError;
use super::check::Check;
use super::names::{escape, relative, short};
use super::probe::{Found, ask};
use super::report::Confirmation;
use crate::import::document::Limits;
use orch_domain::RunningEntry;
use std::collections::BTreeSet;
use std::ffi::OsStr;

use crate::data_dir::confined::ConfinedDir;

impl Check<'_> {
    pub(super) fn state(
        &mut self,
        orch: &ConfinedDir,
        present: bool,
    ) -> Result<(), PreflightError> {
        let name = OsStr::new("state.json");
        let path = relative(&[name]);
        let mut entries: Vec<RunningEntry> = Vec::new();
        if present
            && let Some(file) = self.acquire(orch, name, &path)
            && let Some(bytes) = self.read(file, Limits::JSON.bytes, &path)?
            && let Some(value) = self.json::<serde_json::Value>(&bytes, &path)
        {
            let state = orch_domain::OrchestratorState::from_raw(Some(&value));
            let counts = self.counts();
            counts.claimed = state.claimed.len() as u64;
            counts.retry_entries = state.retry_queue.len() as u64;
            entries = self.running(&value, &path);
        }
        self.counts().running = entries.len() as u64;
        self.in_flight(&entries);
        Ok(())
    }

    /// 1.0 reads a `running` set it cannot decode as an empty one. The check must not:
    /// a set it cannot read could hide a run that is still at work, so it stops.
    fn running(&mut self, value: &serde_json::Value, path: &str) -> Vec<RunningEntry> {
        let Some(running) = value.get("running") else {
            return Vec::new();
        };
        let Some(map) = running.as_object() else {
            self.refuse(
                path,
                "unreadable",
                None,
                None,
                "the running set is not an object; 1.0 reads such a set as empty",
            );
            return Vec::new();
        };
        let mut entries = Vec::with_capacity(map.len());
        for entry in map.values() {
            match serde_json::from_value::<RunningEntry>(entry.clone()) {
                Ok(entry) => entries.push(entry),
                Err(e) => {
                    self.refuse(
                        path,
                        "unreadable",
                        None,
                        None,
                        &format!(
                            "a running entry does not decode ({e}); 1.0 reads the whole set as empty"
                        ),
                    );
                    return Vec::new();
                }
            }
        }
        entries
    }

    fn in_flight(&mut self, entries: &[RunningEntry]) {
        let path = "state.json";
        let named = &self.options.assume_stopped;
        let mut matched = BTreeSet::new();
        for entry in entries {
            let run = escape(OsStr::new(&entry.run_id));
            let task = escape(OsStr::new(&entry.task_id));
            let assumed = named.contains(&entry.run_id);
            if assumed {
                matched.insert(entry.run_id.clone());
            }
            if let Some(remote) = &entry.remote {
                if assumed {
                    self.report.assumed_stopped.push(run);
                    self.counts().in_flight_to_close += 1;
                } else {
                    self.refuse_safe(
                        path,
                        "in_flight_remote",
                        format!(
                            "recorded on peer {}; run {}; task {}",
                            short(&remote.alias),
                            short(&entry.run_id),
                            short(&entry.task_id)
                        ),
                    );
                }
                continue;
            }
            let found: Found = ask(self.probe, entry.pid);
            let detail = format!(
                "pid {}: {}; run {}; task {}",
                entry.pid,
                found.describe(),
                short(&entry.run_id),
                short(&entry.task_id)
            );
            if !found.any() {
                if assumed {
                    self.report.assumed_stopped.push(run);
                }
                self.counts().in_flight_to_close += 1;
            } else if assumed {
                self.report.confirmations.push(Confirmation {
                    run_id: run,
                    task_id: task,
                    pid: entry.pid,
                    found: found.describe(),
                });
                self.counts().in_flight_to_close += 1;
            } else {
                self.refuse_safe(path, "in_flight_live", detail);
            }
        }
        for run in self.options.assume_stopped.clone() {
            if !matched.contains(&run) {
                self.refuse(
                    path,
                    "assume_stopped_unknown",
                    None,
                    Some(escape(OsStr::new(&run))),
                    "no running entry has this run id",
                );
            }
        }
    }
}
