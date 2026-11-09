use std::{fs, path::Path};

#[test]
fn source_has_no_process_capability_tripwire() {
    fn scan(path: &Path) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path);
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let src = fs::read_to_string(&path).unwrap();
            assert!(!src.contains("std::process"), "{}", path.display());
            for token in src.split(|c: char| !c.is_alphanumeric() && c != '_') {
                assert!(
                    ![
                        "Command",
                        "fork",
                        "exec",
                        "execl",
                        "execv",
                        "execve",
                        "posix_spawn"
                    ]
                    .contains(&token),
                    "{}: {token}",
                    path.display()
                );
            }
        }
    }
    scan(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
}
