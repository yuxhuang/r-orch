use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Component, Path};

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

use super::MAX_INPUT;

const MAX_PACKAGES: usize = 128;
const MAX_DEPENDENCIES: usize = 8192;

pub(super) struct Graph {
    roots: BTreeMap<String, String>,
    reverse: BTreeMap<String, BTreeSet<String>>,
}

impl Graph {
    pub(super) fn parse(input: &str) -> Result<Self> {
        ensure!(input.len() <= MAX_INPUT, "metadata exceeds input bound");
        // Compatibility exemption (ARCHITECTURE §4.8): the frozen interface uses
        // Cargo's format-version=1 JSON and the existing serde_json dependency.
        // Only its bounded workspace/dependency fields are projected here.
        let data: Value = serde_json::from_str(input).context("malformed metadata JSON")?;
        if let Some(version) = data.get("version") {
            ensure!(version.as_u64() == Some(1), "unsupported metadata version");
        }
        let root = absolute(text(&data, "workspace_root")?)?;
        let members = data["workspace_members"]
            .as_array()
            .context("metadata has no workspace members")?;
        ensure!(
            !members.is_empty() && members.len() <= MAX_PACKAGES,
            "invalid workspace member count"
        );
        let packages = data["packages"]
            .as_array()
            .context("metadata has no packages")?;
        ensure!(
            packages.len() <= MAX_PACKAGES,
            "metadata exceeds package bound"
        );
        let mut ids = BTreeMap::new();
        for package in packages {
            let id = text(package, "id")?;
            ensure!(ids.insert(id, package).is_none(), "duplicate package id");
        }
        let mut roots = BTreeMap::new();
        let mut selected = BTreeMap::new();
        let mut seen = BTreeSet::new();
        for member in members {
            let id = member.as_str().context("invalid workspace member id")?;
            ensure!(seen.insert(id), "duplicate workspace member");
            let package = ids
                .get(id)
                .context("workspace member missing from packages")?;
            let name = text(package, "name")?;
            ensure!(valid_name(name), "invalid package name");
            let manifest = absolute(text(package, "manifest_path")?)?;
            ensure!(
                manifest.file_name().is_some_and(|s| s == "Cargo.toml"),
                "invalid member manifest"
            );
            let directory = manifest.parent().context("member manifest has no parent")?;
            let relative = directory
                .strip_prefix(root)
                .context("member outside workspace")?;
            let relative = relative
                .to_str()
                .context("non-UTF-8 member root")?
                .to_owned();
            ensure!(
                roots.insert(relative, name.to_owned()).is_none(),
                "duplicate member directory"
            );
            ensure!(
                selected.insert(name.to_owned(), *package).is_none(),
                "ambiguous workspace package name"
            );
        }
        let mut reverse: BTreeMap<String, BTreeSet<String>> = selected
            .keys()
            .map(|name| (name.clone(), BTreeSet::new()))
            .collect();
        let mut count = 0;
        for (name, package) in &selected {
            let dependencies = package["dependencies"]
                .as_array()
                .context("missing package dependencies")?;
            count += dependencies.len();
            ensure!(
                count <= MAX_DEPENDENCIES,
                "metadata exceeds dependency bound"
            );
            for dependency in dependencies {
                let dep_name = text(dependency, "name")?;
                match dependency.get("kind") {
                    None | Some(Value::Null) => {}
                    Some(Value::String(kind))
                        if kind == "normal" || kind == "build" || kind == "dev" => {}
                    _ => bail!("unknown dependency kind"),
                }
                match dependency.get("source") {
                    Some(Value::String(source)) => {
                        ensure!(
                            ["registry+", "sparse+", "git+"]
                                .iter()
                                .any(|prefix| source.starts_with(prefix)),
                            "unknown dependency source"
                        );
                        continue; // Registry/git homonyms are not workspace edges.
                    }
                    None | Some(Value::Null) => {}
                    _ => bail!("invalid dependency source"),
                }
                if let Some(path) = dependency.get("path").filter(|path| !path.is_null()) {
                    let path = absolute(path.as_str().context("invalid dependency path")?)?;
                    let owner = path
                        .strip_prefix(root)
                        .ok()
                        .and_then(Path::to_str)
                        .and_then(|relative| roots.get(relative));
                    match owner {
                        Some(owner) => ensure!(owner == dep_name, "dependency name/path mismatch"),
                        None => {
                            ensure!(
                                !selected.contains_key(dep_name),
                                "ambiguous member dependency path"
                            );
                            continue;
                        }
                    }
                }
                if let Some(dependents) = reverse.get_mut(dep_name) {
                    // Include optional and target-specific edges as well: no active
                    // feature or host projection is allowed to narrow test coverage.
                    dependents.insert(name.clone());
                }
            }
        }
        Ok(Self { roots, reverse })
    }

    pub(super) fn contains(&self, name: &str) -> bool {
        self.reverse.contains_key(name)
    }

    pub(super) fn owner(&self, path: &str) -> Option<&str> {
        self.roots
            .iter()
            .filter(|(root, _)| {
                root.is_empty()
                    || path
                        .strip_prefix(root.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            })
            .max_by_key(|(root, _)| root.len())
            .map(|(_, name)| name.as_str())
    }

    pub(super) fn dependents(&self, name: &str) -> BTreeSet<String> {
        let mut result = BTreeSet::from([name.to_owned()]);
        let mut queue = VecDeque::from([name.to_owned()]);
        while let Some(next) = queue.pop_front() {
            if let Some(dependents) = self.reverse.get(&next) {
                for dependent in dependents {
                    if result.insert(dependent.clone()) {
                        queue.push_back(dependent.clone());
                    }
                }
            }
        }
        result
    }
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    let text = value
        .get(key)
        .and_then(Value::as_str)
        .context("missing metadata string")?;
    ensure!(
        !text.is_empty() && text.len() <= super::MAX_PATH && !text.contains('\0'),
        "invalid metadata string"
    );
    Ok(text)
}

fn absolute(value: &str) -> Result<&Path> {
    let path = Path::new(value);
    ensure!(
        path.is_absolute() && value.len() <= super::MAX_PATH,
        "metadata path is not bounded and absolute"
    );
    ensure!(
        !value.contains('\0') && !path.components().any(|c| matches!(c, Component::ParentDir)),
        "invalid metadata path"
    );
    Ok(path)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && name.as_bytes()[0].is_ascii_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate_selection::tests::metadata;

    #[test]
    fn missing_members_ambiguous_roots_and_unknown_dependency_kinds_reject() {
        let original: Value = serde_json::from_str(&metadata()).unwrap();
        let mut missing = original.clone();
        missing["workspace_members"][0] = Value::String("missing".into());
        let mut duplicate = original.clone();
        duplicate["packages"][1]["manifest_path"] =
            duplicate["packages"][0]["manifest_path"].clone();
        let mut kind = original.clone();
        kind["packages"][1]["dependencies"][0]["kind"] = Value::String("future".into());
        for invalid in [missing, duplicate, kind] {
            assert!(Graph::parse(&invalid.to_string()).is_err());
        }
    }

    #[test]
    fn registry_homonyms_do_not_add_workspace_edges() {
        let mut data: Value = serde_json::from_str(&metadata()).unwrap();
        data["packages"][1]["dependencies"][0]["source"] = Value::String("registry+example".into());
        let graph = Graph::parse(&data.to_string()).unwrap();
        assert_eq!(
            graph.dependents("orch-domain"),
            BTreeSet::from(["orch-domain".into()])
        );
    }
}
