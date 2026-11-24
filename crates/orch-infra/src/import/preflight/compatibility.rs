//! Read-side reconciliation. Never open names found inside an index or config.
use super::*;
use std::collections::BTreeSet;

/// The source is bounded by one index's JSON budget. Entity files are checked one at
/// a time; their contents are not accumulated across a directory.
pub(super) struct IndexPlan {
    entries: BTreeMap<String, (u64, Box<RawValue>)>,
    order: BTreeMap<u64, String>,
    next: u64,
    usable: bool,
    stopped: bool,
    omitted: u64,
}

enum IndexRead {
    Value(Vec<Box<RawValue>>),
    Unreadable,
    Stopped,
}

impl Check<'_> {
    pub(super) fn index_plan(
        &mut self,
        store: Store,
        level: &Level,
    ) -> Result<IndexPlan, PreflightError> {
        let name = OsStr::new("_index.json");
        let path = relative(&[OsStr::new(store.dir()), name]);
        let mut plan = IndexPlan {
            entries: BTreeMap::new(),
            order: BTreeMap::new(),
            next: 1,
            usable: false,
            stopped: false,
            omitted: 0,
        };
        let value = if level.names.iter().any(|n| n == name) {
            let Some(file) = self.acquire(&level.dir, name, &path) else {
                plan.stopped = true;
                return Ok(plan);
            };
            match self.index_read(file, &path)? {
                IndexRead::Value(value) => Some(value),
                IndexRead::Unreadable => None,
                IndexRead::Stopped => {
                    plan.stopped = true;
                    return Ok(plan);
                }
            }
        } else {
            None
        };
        let Some(entries) = value else {
            self.note(
                &path,
                "index_unreadable",
                None,
                "missing or unusable index; entity files are ordered by id",
            );
            return Ok(plan);
        };
        let key = if store == Store::Context { "key" } else { "id" };
        // Extract identities without judging other fields. RawValue preserves
        // valid ignored numbers that a generic Value cannot represent.
        let identities: Vec<_> = entries
            .iter()
            .map(|entry| {
                serde_json::from_str::<BTreeMap<String, &RawValue>>(entry.get())
                    .ok()
                    .and_then(|map| {
                        map.get(key)
                            .and_then(|id| serde_json::from_str::<String>(id.get()).ok())
                    })
            })
            .collect();
        // Whole-array precedence is part of R122, independent of entry order.
        if identities.iter().any(Option::is_none) {
            self.refuse(&path, "index_entry", None, None, "index entry has no string identity; 1.0 rebuilt such an index; after taking a copy, remove the index to accept id order");
            plan.stopped = true;
            return Ok(plan);
        }
        let mut seen = BTreeSet::new();
        for id in &identities {
            self.cancelled()?;
            let id = id.as_ref().expect("checked whole array");
            if !seen.insert(id) {
                self.refuse(&path, "index_duplicate", None, Some(bounded(id)), "repeated identity; 1.0 used such an index only if every entry was a complete entity of this directory's type, and otherwise rebuilt it; after taking a copy, remove the extra entry or remove the index to accept id order");
                plan.stopped = true;
                return Ok(plan);
            }
        }
        plan.usable = true;
        plan.next = entries.len() as u64 + 1;
        for (position, (id, entry)) in identities.into_iter().zip(entries).enumerate() {
            plan.entries.insert(
                id.expect("checked whole array"),
                (position as u64 + 1, entry),
            );
        }
        Ok(plan)
    }

    fn index_read(&mut self, reader: impl Read, path: &str) -> Result<IndexRead, PreflightError> {
        let Some(bytes) = self.read(reader, Limits::JSON.bytes, path)? else {
            return Ok(IndexRead::Stopped);
        };
        match check_json(&bytes, Limits::JSON) {
            Ok(()) => {
                self.counts().indexes += 1;
                if bytes.iter().copied().find(|b| !b.is_ascii_whitespace()) != Some(b'[') {
                    return Ok(IndexRead::Unreadable);
                }
                let entries = serde_json::from_slice::<Vec<Box<RawValue>>>(&bytes)
                    .expect("JSON budget pass checked an array's syntax");
                Ok(IndexRead::Value(entries))
            }
            Err(r) if r.rule == "parse" => {
                self.counts().indexes += 1;
                Ok(IndexRead::Unreadable)
            }
            Err(r) => {
                self.refusal(path, r);
                Ok(IndexRead::Stopped)
            }
        }
    }

    pub(super) fn index_invalid_file(&mut self, plan: &mut IndexPlan, stem: &[u8]) {
        // The file has already produced its refusal. It exists, so do not also
        // tell the owner that its index names a file that is missing.
        if let Ok(stem) = std::str::from_utf8(stem) {
            plan.entries.remove(stem);
        }
    }

    pub(super) fn index_file(
        &mut self,
        store: &'static str,
        plan: &mut IndexPlan,
        stem: &[u8],
        value: &DocumentContent,
    ) {
        if plan.stopped {
            return;
        }
        let id = std::str::from_utf8(stem).expect("validated ASCII stem");
        let path = format!("{store}/_index.json");
        let position = match plan.entries.remove(id) {
            Some((position, entry)) => {
                if !value.matches(&entry) {
                    self.note(
                        &path,
                        "index_differs",
                        Some(bounded(id)),
                        "entity content is taken from its file, not its index",
                    );
                }
                position
            }
            None => {
                if plan.usable {
                    self.note(
                        &path,
                        "index_unlisted",
                        Some(bounded(id)),
                        "entity file is not listed in its index; appended in id order",
                    );
                }
                let position = plan.next;
                plan.next += 1;
                position
            }
        };
        plan.order.insert(position, id.to_owned());
        if plan.order.len() > crate::import::preflight::REPORT_LIMIT {
            plan.order.pop_last();
            plan.omitted += 1;
        }
    }

    pub(super) fn finish_index(&mut self, store: &'static str, plan: IndexPlan) {
        if plan.stopped {
            return;
        }
        let path = format!("{store}/_index.json");
        for id in plan.entries.keys() {
            self.note(
                &path,
                "index_missing_file",
                Some(bounded(id)),
                "index entry has no entity file; skipped without opening its identity as a path",
            );
        }
        self.report.order.more += plan.omitted;
        for (position, id) in plan.order {
            self.report.order.push(OrderedEntity {
                store,
                id: bounded(&id),
                position,
            });
        }
    }

    pub(super) fn config_unknown_notes(&mut self, path: &str, value: &Value, prefix: &str) {
        if let Some(map) = value.as_object() {
            for (key, value) in map {
                // Bound each component before concatenating, including at every level.
                let key = if prefix.is_empty() {
                    key.chars().take(DETAIL_CHARS).collect()
                } else {
                    format!(
                        "{prefix}.{}",
                        key.chars().take(DETAIL_CHARS).collect::<String>()
                    )
                };
                if value.is_object() && !value.as_object().expect("object").is_empty() {
                    self.config_unknown_notes(path, value, &key);
                } else {
                    self.note(path, "unknown_config", Some(bounded(&key)), "unknown key is kept inert in legacy.config; its value is never interpreted");
                }
            }
        }
    }
}

/// Frozen 1.0 defaults (fb6a1e1), independent of later product defaults. Optional
/// keys are present as null so unknown-key classification knows their names too.
pub(super) fn legacy_defaults() -> Value {
    serde_json::json!({
        "project": {"name":"my-project", "description":null},
        "defaults": {
            "agent": {"adapter":"claude", "approval_policy":"auto", "max_turns":50,
                "timeout_ms":3600000, "stall_timeout_ms":600000, "workspace_mode":"worktree", "prompt_mode":"normal"},
            "task": {"max_attempts":3, "priority":3}
        },
        "scheduling": {"poll_interval_ms":10000, "max_concurrent_agents":6,
            "retry_base_delay_ms":10000, "retry_max_delay_ms":300000},
        "prompt": null,
        "peer": {"name":null,"listen":null}
    })
}

pub(super) fn unknown_config(value: &Value, schema: &Value) -> Value {
    let Some(map) = value.as_object() else {
        return serde_json::json!({});
    };
    let mut unknown = serde_json::Map::new();
    for (key, value) in map {
        let expected = if key == "prompt" && schema.get(key).is_some_and(Value::is_null) {
            Some(serde_json::json!({"template":null,"system_template":null,"user_template":null}))
        } else {
            schema.get(key).cloned()
        };
        match expected {
            None => {
                unknown.insert(key.clone(), value.clone());
            }
            Some(expected) if expected.is_object() => {
                let child = unknown_config(value, &expected);
                if child.as_object().is_some_and(|m| !m.is_empty()) {
                    unknown.insert(key.clone(), child);
                }
            }
            _ => {}
        }
    }
    Value::Object(unknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fault {
        bytes: &'static [u8],
        offset: usize,
        fired: bool,
        kind: io::ErrorKind,
    }
    impl Read for Fault {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.offset == 2 && !self.fired {
                self.fired = true;
                return Err(self.kind.into());
            }
            let n = out.len().min(2).min(self.bytes.len() - self.offset);
            out[..n].copy_from_slice(&self.bytes[self.offset..self.offset + n]);
            self.offset += n;
            Ok(n)
        }
    }

    #[test]
    fn an_index_read_error_stops_and_an_interrupted_read_is_retried() {
        let options = Options::default();
        let probe = crate::import::preflight::SystemProbe;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        for kind in [io::ErrorKind::Interrupted, io::ErrorKind::PermissionDenied] {
            let mut check = Check {
                options: &options,
                probe: &probe,
                runtime: &runtime,
                report: Report::default(),
                listed: 0,
                tasks: BTreeMap::new(),
                auto_agent: false,
                inherited_agent_policy: false,
            };
            let fault = Fault {
                bytes: br#"[{"id":"b"}]"#,
                offset: 0,
                fired: false,
                kind,
            };
            let result = check.index_read(fault, "tasks/_index.json").unwrap();
            if kind == io::ErrorKind::Interrupted {
                assert!(matches!(result, IndexRead::Value(_)));
                assert!(!check.report.refused());
            } else {
                assert!(matches!(result, IndexRead::Stopped));
                assert_eq!(check.report.refusals.listed[0].rule, "io");
                assert_eq!(check.report.refusals.listed[0].path, "tasks/_index.json");
            }
            assert!(
                !check
                    .report
                    .notes
                    .listed
                    .iter()
                    .any(|f| f.rule == "index_unreadable")
            );
        }
    }
}
