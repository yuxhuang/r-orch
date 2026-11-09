use orch_client::{ClientConfig, Project};
use orch_proto::handshake::{Caller, CallerKind, SessionMode};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

pub fn config(root: &Path, socket: &Path) -> ClientConfig {
    ClientConfig {
        cwd: root.into(),
        project_root: Some(root.into()),
        socket: Some(socket.into()),
        test_socket_root: socket.parent().map(Into::into),
        agent_context: false,
        caller: Caller {
            kind: CallerKind::Cli,
            agent_id: None,
            pid: None,
        },
        mode: SessionMode::Normal,
    }
}
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join(".orchestry")).unwrap();
    dir
}

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

#[test]
fn explicit_project_wins_over_nested_cwd_and_aliases_keep_identity() {
    let a = project();
    let b = project();
    let expected = Project::discover(a.path(), None).unwrap();
    let selected = Project::discover(b.path(), Some(a.path())).unwrap();
    assert_eq!(expected.id, selected.id);
    assert_ne!(expected.id, Project::discover(b.path(), None).unwrap().id);
    let link_dir = tempfile::tempdir().unwrap();
    let alias = link_dir.path().join("alias");
    symlink(a.path(), &alias).unwrap();
    assert_eq!(Project::discover(&alias, None).unwrap().id, expected.id);
    assert!(Project::discover(a.path(), Some(&a.path().join("missing"))).is_err());
}

#[test]
fn symlink_state_directory_is_never_an_initialized_project() {
    let a = project();
    let b = tempfile::tempdir().unwrap();
    symlink(a.path().join(".orchestry"), b.path().join(".orchestry")).unwrap();
    assert_eq!(
        Project::discover(b.path(), None).unwrap_err().exit_code(),
        3
    );
}

#[test]
fn discovery_rejects_unsafe_modes_symlinks_fifos_and_oversize_without_hanging() {
    let dir = project();
    let project = Project::discover(dir.path(), None).unwrap();
    let record = dir.path().join(".orchestry/daemon.json");
    fs::write(&record, "{}").unwrap();
    fs::set_permissions(&record, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        project
            .read_record()
            .unwrap_err()
            .to_string()
            .contains("safety checks")
    );
    fs::remove_file(&record).unwrap();
    symlink("/dev/null", &record).unwrap();
    assert!(project.read_record().is_err());
    fs::remove_file(&record).unwrap();
    nix::unistd::mkfifo(&record, nix::sys::stat::Mode::S_IRUSR).unwrap();
    assert!(project.read_record().is_err());
    fs::remove_file(&record).unwrap();
    fs::write(&record, vec![b' '; 16 * 1024 + 1]).unwrap();
    fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(project.read_record().is_err());
}

#[tokio::test]
async fn test_root_confinement_checks_components_and_resolves_symlink_parents() {
    let p = project();
    let base = tempfile::Builder::new()
        .prefix("oc-")
        .tempdir_in("/tmp")
        .unwrap();
    let allowed = base.path().join("allowed");
    let sibling = base.path().join("allowed-sibling");
    fs::create_dir(&allowed).unwrap();
    fs::create_dir(&sibling).unwrap();
    let link = allowed.join("escape");
    symlink(&sibling, &link).unwrap();
    for socket in [
        sibling.join("s"),
        link.join("s"),
        allowed.join("../allowed-sibling/s"),
    ] {
        let mut cfg = config(p.path(), &socket);
        cfg.test_socket_root = Some(allowed.clone());
        let err = orch_client::Client::connect(&cfg).await.err().unwrap();
        assert!(
            err.to_string()
                .contains("refusing to connect outside the test socket root"),
            "{err}"
        );
    }
    let socket = allowed.join("s");
    let mut cfg = config(p.path(), &socket);
    cfg.test_socket_root = Some(allowed);
    let err = orch_client::Client::connect(&cfg).await.err().unwrap();
    assert_eq!(err.exit_code(), 7);
    assert!(err.to_string().contains("no socket"));
}

#[test]
fn safe_record_is_read_from_the_pinned_state_directory_and_wrong_identity_is_rejected() {
    use orch_proto::bounded::{BoundedString, Id};
    use orch_proto::discovery::DaemonRecord;
    use orch_proto::path::WirePath;
    let root = project();
    let selected = Project::discover(root.path(), None).unwrap();
    let mut record = DaemonRecord {
        schema: 1,
        project_root: WirePath::new(selected.root.clone()).unwrap(),
        project_id: selected.id,
        socket: WirePath::new("/tmp/unused-test-socket").unwrap(),
        pid: 42,
        binary: BoundedString::new(orch_proto::BINARY_VERSION).unwrap(),
        protocol: orch_proto::LOCAL_PROTOCOL,
        executable: WirePath::new("/unused-test-binary").unwrap(),
        started_at: Id::new("now").unwrap(),
        instance: Id::new("test-instance").unwrap(),
    };
    let path = root.path().join(".orchestry/daemon.json");
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(selected.read_record().unwrap(), record);
    fs::rename(
        root.path().join(".orchestry"),
        root.path().join("old-state"),
    )
    .unwrap();
    fs::create_dir(root.path().join(".orchestry")).unwrap();
    // A swapped path cannot redirect the already selected project's read.
    assert_eq!(selected.read_record().unwrap(), record);
    record.project_id.ino += 1;
    fs::write(
        root.path().join("old-state/daemon.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let err = selected.read_record().unwrap_err();
    assert_eq!(err.exit_code(), 7);
    assert!(err.to_string().contains("wrong_project"));
}
