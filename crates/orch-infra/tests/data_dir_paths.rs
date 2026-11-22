use orch_infra::data_dir::paths::{
    default_name, read_project_record, resolve, write_project_record,
};
use std::os::unix::fs::{PermissionsExt, symlink};

#[test]
fn the_project_record_is_named_by_the_hash_of_the_id() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let data = tmp.path().join("data");
    std::fs::create_dir(&data).unwrap();
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
    write_project_record(&home, "prj_test1", &data).unwrap();
    assert!(
        home.join("projects/9fa0423cd44dfbc56bc5b71dea05c66fb0f1f2f0d884eed107e15cfdbce760c7")
            .is_file()
    );
    assert_eq!(read_project_record(&home, "prj_test1").unwrap(), data);
    assert!(write_project_record(&home, "../escape", &data).is_err());
    assert_eq!(resolve(None, &home, "prj_test1").unwrap(), data);
    assert!(resolve(Some(std::path::Path::new("relative")), &home, "prj_test1").is_err());
}

#[test]
fn a_record_that_is_not_one_absolute_path_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let data = tmp.path().join("data");
    std::fs::create_dir(&data).unwrap();
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
    write_project_record(&home, "prj_test1", &data).unwrap();
    let record =
        home.join("projects/9fa0423cd44dfbc56bc5b71dea05c66fb0f1f2f0d884eed107e15cfdbce760c7");
    for bad in [
        b"relative\n".to_vec(),
        b"/one\n/two\n".to_vec(),
        vec![b'a'; 4097],
        b"/nul\0path\n".to_vec(),
    ] {
        std::fs::write(&record, bad).unwrap();
        assert!(read_project_record(&home, "prj_test1").is_err());
    }
    std::fs::remove_file(&record).unwrap();
    let canary = tmp.path().join("canary");
    std::fs::write(&canary, format!("{}\n", data.display())).unwrap();
    symlink(&canary, &record).unwrap();
    assert!(read_project_record(&home, "prj_test1").is_err());
    assert!(write_project_record(&home, "prj_test1", &data).is_err());
    assert_eq!(
        std::fs::read_to_string(canary).unwrap(),
        format!("{}\n", data.display())
    );
}

#[test]
fn an_existing_record_is_never_retargeted() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    for name in ["first", "other"] {
        std::fs::create_dir(tmp.path().join(name)).unwrap();
        std::fs::set_permissions(
            tmp.path().join(name),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    write_project_record(&home, "prj_test1", &tmp.path().join("first")).unwrap();
    write_project_record(&home, "prj_test1", &tmp.path().join("first")).unwrap();
    assert!(write_project_record(&home, "prj_test1", &tmp.path().join("other")).is_err());
    assert_eq!(
        read_project_record(&home, "prj_test1").unwrap(),
        tmp.path().join("first")
    );
    std::fs::set_permissions(
        tmp.path().join("first"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(read_project_record(&home, "prj_test1").is_err());
}

#[test]
fn default_name_is_derived_from_the_project_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("My Project");
    std::fs::create_dir(&project).unwrap();
    let name = default_name(&project).unwrap();
    assert!(name.starts_with("my-project-"), "{name}");
    assert_eq!(name.len(), 10 + 1 + 12);
    assert_eq!(name, default_name(&project.join(".")).unwrap());
}
