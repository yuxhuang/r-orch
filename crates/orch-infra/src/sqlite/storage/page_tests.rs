use super::page;
use crate::blob::DiskBlobStore;
use crate::sqlite::{
    ConnectionRole, Synchronous, migrations, non_task_cursor::NonTaskCursors, open_connection,
};
use orch_core::store::{Entity, Filter, PageRequest, Sort};
use serde_json::json;

#[test]
fn team_pages_resume_original_tuple_with_count_and_cross_sort_refusal() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrations::migrate(&data, &mut conn).unwrap();
    let tx = conn.transaction().unwrap();
    let mut sql = crate::sqlite::agent_team_run::NativeSql::new(&tx, &disk, "p", 0);
    for (id, name) in [("a", "a"), ("b", "b"), ("c", "c")] {
        sql.create(Entity::Team(Box::new(serde_json::from_value(json!({"id":id,"name":name,"status":"active","members":[],"task_pool":[],"lead_agent_id":"a","created_at":name,"updated_at":"opaque","config":{"auto_claim":false}})).unwrap()))).unwrap();
    }
    let cursors = NonTaskCursors::new("g");
    let request = PageRequest {
        limit: 1,
        cursor: None,
        sort: Sort::Name,
        with_count: true,
    };
    let a = page::page(&tx, &disk, "p", &cursors, 0, &Filter::Teams, &request).unwrap();
    assert_eq!(a.items[0].entity.id(), "a");
    let next = PageRequest {
        cursor: a.next_cursor,
        ..request
    };
    let b = page::page(&tx, &disk, "p", &cursors, 0, &Filter::Teams, &next).unwrap();
    assert_eq!(b.items[0].entity.id(), "b");
    assert_eq!(b.total, Some(3));
    assert!(matches!(
        page::page(
            &tx,
            &disk,
            "p",
            &cursors,
            0,
            &Filter::Teams,
            &PageRequest {
                sort: Sort::Default,
                ..next
            }
        ),
        Err(crate::sqlite::SqliteError::Store(
            orch_core::store::StoreError::InvalidCursor
        ))
    ));
}
