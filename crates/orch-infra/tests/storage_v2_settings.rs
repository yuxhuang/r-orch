//! What only the file reference can get wrong for settings and templates: the
//! document on disk. The port's rules for these families are the shared
//! scenarios of `orch-engine`'s `storage_conformance`.

use std::path::Path;

use orch_core::store::*;
use orch_core::test_support::assert_hermetic;
use orch_infra::storage::v2::FileStorage;

fn owner(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "owner".into(),
        },
    }
}

fn setting(key: &str, value: &str) -> Entity {
    Entity::Config(ConfigEntry {
        key: key.into(),
        value: value.into(),
    })
}

fn template(name: &str, body: &str) -> Entity {
    Entity::Template(TemplateEntry {
        name: name.into(),
        body: body.into(),
    })
}

fn root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    assert_hermetic(root.path());
    root
}

async fn create(s: &FileStorage, project: &str, value: Entity) -> Record {
    s.transaction(owner(project), Lane::Entities, move |u| u.create(value))
        .await
        .unwrap()
}

async fn get(s: &FileStorage, project: &str, kind: EntityKind, id: &str) -> Option<Record> {
    s.read(owner(project))
        .await
        .unwrap()
        .get(kind, id)
        .await
        .unwrap()
}

/// Every path below `dir`, relative to it, directories included, in order.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            found.push(path.strip_prefix(base).unwrap().display().to_string());
            if path.is_dir() {
                walk(base, &path, found);
            }
        }
    }
    let mut found = vec![];
    walk(dir, dir, &mut found);
    found.sort();
    found
}

#[tokio::test]
async fn settings_and_templates_survive_a_new_instance_with_their_counters() {
    let root = root();
    let first = FileStorage::new(root.path());
    create(&first, "p", setting("run_cap", "2")).await;
    create(&first, "p", setting("preapproval", "on")).await;
    create(&first, "p", template("default", "hello")).await;
    first
        .write(
            owner("p"),
            Lane::Entities,
            Box::new(|u| {
                u.update(setting("run_cap", "3"), Revision::ONE)?;
                u.delete(EntityKind::Config, "preapproval", Revision::ONE)
            }),
        )
        .await
        .unwrap();
    drop(first);

    let second = FileStorage::new(root.path());
    let run_cap = get(&second, "p", EntityKind::Config, "run_cap")
        .await
        .unwrap();
    assert_eq!(run_cap.entity, setting("run_cap", "3"));
    assert_eq!((run_cap.revision.get(), run_cap.position), (2, 1));
    assert_eq!(
        get(&second, "p", EntityKind::Config, "preapproval").await,
        None
    );
    let default = get(&second, "p", EntityKind::Template, "default")
        .await
        .unwrap();
    assert_eq!(default.entity, template("default", "hello"));
    assert_eq!((default.revision.get(), default.position), (1, 1));
    // Each family's counter outlives its deleted row, and neither borrows from
    // the other, so the next setting is 3 and the next template 2.
    assert_eq!(
        create(&second, "p", setting("later", "x")).await.position,
        3
    );
    assert_eq!(
        create(&second, "p", template("later", "x")).await.position,
        2
    );
}

#[tokio::test]
async fn a_setting_key_and_a_template_name_are_data_and_never_a_path() {
    let parent = root();
    let store = parent.path().join("store");
    std::fs::create_dir(&store).unwrap();
    let s = FileStorage::new(&store);
    let names = ["../escape", "a/b", "/abs", "..", ".", "\0", "p-70", "~1"];
    for name in names {
        create(&s, "p", setting(name, name)).await;
        create(&s, "p", template(name, name)).await;
    }
    // One project, one document, and nothing beside the store: no name left the
    // project's directory, and none made a file of its own.
    assert_eq!(
        tree(parent.path()),
        ["store", "store/p-70", "store/p-70/state.json"]
    );
    for name in names {
        for (kind, entity) in [
            (EntityKind::Config, setting(name, name)),
            (EntityKind::Template, template(name, name)),
        ] {
            let row = get(&s, "p", kind, name).await.unwrap();
            assert_eq!(row.entity, entity, "{kind:?} {name:?}");
        }
    }
    assert_eq!(get(&s, "p", EntityKind::Template, "../other").await, None);
}
