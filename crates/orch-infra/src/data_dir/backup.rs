//! R108 name/set checks only. No restore or manifest parsing is activated here.
use super::{
    DataDirError,
    confined::{ConfinedDir, FileAccess},
};
use std::collections::BTreeSet;

fn invalid() -> DataDirError {
    DataDirError::InvalidLocation(
        "backup inventory does not match the 1.1 layout or manifest".into(),
    )
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn blob_name(name: &str) -> bool {
    // Bound the split before allocating, even for hostile manifest text.
    if name.len() != 83 {
        return false;
    }
    let parts: Vec<_> = name.split('/').collect();
    matches!(parts.as_slice(), ["blobs", "sha256", ab, cd, hash]
        if hex(hash, 64) && *ab == &hash[..2] && *cd == &hash[2..4])
}

fn fixed_entries(dir: &ConfinedDir, expected: &[&str]) -> Result<(), DataDirError> {
    let mut expected: Vec<_> = expected.iter().map(std::ffi::OsString::from).collect();
    expected.sort();
    if dir.list()? != expected {
        return Err(invalid());
    }
    Ok(())
}

/// Check a full 1.1 backup's file inventory. Manifest names are data only:
/// `orch.db` and blob file names, excluding the manifest itself and directories.
/// Only fixed names and validated listing results select descriptor operations.
///
/// This does not parse a manifest, verify sizes/hashes or database contents, or
/// establish restore eligibility. Database-only migration copies are not full
/// backups and are outside this helper. The caller must keep the tree stable;
/// a listing is not a snapshot or protection against same-user replacement.
pub fn validate_backup_inventory(
    root: &ConfinedDir,
    manifest_names: &[String],
) -> Result<(), DataDirError> {
    let mut expected = BTreeSet::new();
    for name in manifest_names {
        if (name != "orch.db" && !blob_name(name)) || !expected.insert(name.as_str()) {
            return Err(invalid());
        }
    }
    fixed_entries(root, &["manifest.json", "orch.db", "blobs", "repos"])?;
    root.check_file("manifest.json", FileAccess::Untrusted)?;
    root.check_file("orch.db", FileAccess::Untrusted)?;
    fixed_entries(&root.open_dir("repos")?, &[])?;
    let blobs = root.open_dir("blobs")?;
    fixed_entries(&blobs, &["sha256"])?;
    let hashes = blobs.open_dir("sha256")?;
    let mut listed = BTreeSet::from(["orch.db".to_owned()]);
    for ab in hashes.list()? {
        let ab = ab
            .to_str()
            .filter(|name| hex(name, 2))
            .ok_or_else(invalid)?;
        let first = hashes.open_dir(ab)?;
        for cd in first.list()? {
            let cd = cd
                .to_str()
                .filter(|name| hex(name, 2))
                .ok_or_else(invalid)?;
            let second = first.open_dir(cd)?;
            for hash in second.list()? {
                let hash = hash
                    .to_str()
                    .filter(|name| hex(name, 64))
                    .ok_or_else(invalid)?;
                if &hash[..2] != ab || &hash[2..4] != cd {
                    return Err(invalid());
                }
                second.check_file(hash, FileAccess::Untrusted)?;
                listed.insert(format!("blobs/sha256/{ab}/{cd}/{hash}"));
            }
        }
    }
    if !listed.iter().map(String::as_str).eq(expected) {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, ConfinedDir) {
        let temp = tempfile::tempdir().unwrap();
        let root = ConfinedDir::open(temp.path()).unwrap();
        root.create_file("manifest.json").unwrap();
        root.create_file("orch.db").unwrap();
        root.create_dir("repos").unwrap();
        root.create_dir("blobs")
            .unwrap()
            .create_dir("sha256")
            .unwrap();
        (temp, root)
    }

    fn names() -> Vec<String> {
        vec!["orch.db".into()]
    }

    #[test]
    fn empty_repositories_and_blob_tree_are_valid() {
        let (_temp, root) = fixture();
        validate_backup_inventory(&root, &names()).unwrap();
    }

    #[test]
    fn missing_and_unexpected_inventory_entries_are_refused() {
        for missing in ["manifest.json", "orch.db", "repos", "blobs"] {
            let (temp, root) = fixture();
            let path = temp.path().join(missing);
            if path.is_dir() {
                std::fs::remove_dir_all(path).unwrap();
            } else {
                std::fs::remove_file(path).unwrap();
            }
            assert!(
                validate_backup_inventory(&root, &names()).is_err(),
                "{missing}"
            );
        }
        for directory in ["", "repos", "blobs", "blobs/sha256"] {
            let (temp, root) = fixture();
            std::fs::write(temp.path().join(directory).join("unexpected"), b"synthetic").unwrap();
            assert!(
                validate_backup_inventory(&root, &names()).is_err(),
                "{directory}"
            );
        }
    }

    #[test]
    fn manifest_duplicates_invalid_names_and_set_differences_are_refused() {
        let (_temp, root) = fixture();
        for invalid in [
            "../outside-canary",
            "/outside-canary",
            "a\0b",
            "",
            ".",
            "..",
            "manifest.json",
            "repos",
            "blobs/events/ab/file",
            "orch.db/",
            "orch.db\\x",
        ] {
            let mut manifest = names();
            manifest.push(invalid.into());
            assert!(
                validate_backup_inventory(&root, &manifest).is_err(),
                "{invalid:?}"
            );
        }
        assert!(validate_backup_inventory(&root, &[]).is_err());
        assert!(validate_backup_inventory(&root, &["orch.db".into(), "orch.db".into()]).is_err());
    }

    #[test]
    fn blob_names_must_match_their_shards_and_manifest() {
        let (_temp, root) = fixture();
        let hash = "abcd0123456789abcdef0123456789abcdef0123456789abcdef0123456789ab";
        let leaf = root
            .open_dir("blobs")
            .unwrap()
            .open_dir("sha256")
            .unwrap()
            .create_dir("ab")
            .unwrap()
            .create_dir("cd")
            .unwrap();
        leaf.create_file(hash).unwrap();
        let mut manifest = names();
        let name = format!("blobs/sha256/ab/cd/{hash}");
        manifest.push(name.clone());
        validate_backup_inventory(&root, &manifest).unwrap();
        let mut duplicate = manifest.clone();
        duplicate.push(name.clone());
        assert!(validate_backup_inventory(&root, &duplicate).is_err());
        assert!(validate_backup_inventory(&root, &names()).is_err());
        for bad in [
            name.to_uppercase(),
            name.replace("/ab/", "/aa/"),
            name.replace("/cd/", "/00/"),
            format!("{name}/extra"),
        ] {
            assert!(validate_backup_inventory(&root, &["orch.db".into(), bad]).is_err());
        }
        leaf.remove_file(hash).unwrap();
        assert!(validate_backup_inventory(&root, &manifest).is_err());
        leaf.create_file("wrong").unwrap();
        assert!(validate_backup_inventory(&root, &names()).is_err());
    }

    #[test]
    fn invalid_listed_shards_and_misplaced_hashes_are_refused() {
        for shard in ["a", "AB", "gg", "a\\b"] {
            let (_temp, root) = fixture();
            root.open_dir("blobs")
                .unwrap()
                .open_dir("sha256")
                .unwrap()
                .create_dir(shard)
                .unwrap();
            assert!(validate_backup_inventory(&root, &names()).is_err());
        }
        let (_temp, root) = fixture();
        let leaf = root
            .open_dir("blobs")
            .unwrap()
            .open_dir("sha256")
            .unwrap()
            .create_dir("ab")
            .unwrap()
            .create_dir("cd")
            .unwrap();
        leaf.create_file("0".repeat(64)).unwrap();
        assert!(validate_backup_inventory(&root, &names()).is_err());
    }

    #[test]
    fn inventory_links_special_files_and_non_utf8_names_are_refused() {
        use std::os::unix::{ffi::OsStrExt, fs::symlink};
        let (temp, root) = fixture();
        root.remove_file("orch.db").unwrap();
        symlink("absent-canary", temp.path().join("orch.db")).unwrap();
        assert!(validate_backup_inventory(&root, &names()).is_err());
        std::fs::remove_file(temp.path().join("orch.db")).unwrap();
        root.create_dir("orch.db").unwrap();
        assert!(validate_backup_inventory(&root, &names()).is_err());
        let (_temp, root) = fixture();
        root.create_file(std::ffi::OsStr::from_bytes(b"\xff"))
            .unwrap();
        assert!(validate_backup_inventory(&root, &names()).is_err());
    }
}
