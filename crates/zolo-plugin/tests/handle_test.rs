use zolo_plugin::handle::{HandleTable, PluginHandle};

#[test]
fn test_handle_create_and_get() {
    let handle = PluginHandle::new(42_i64, "TestInt");
    let val = handle.get().unwrap();
    assert_eq!(*val, 42);
}

#[test]
fn test_handle_get_mut() {
    let handle = PluginHandle::new(vec![1, 2, 3], "TestVec");
    {
        let mut val = handle.get_mut().unwrap();
        val.push(4);
    }
    let val = handle.get().unwrap();
    assert_eq!(*val, vec![1, 2, 3, 4]);
}

#[test]
fn test_handle_close() {
    let handle = PluginHandle::new(String::from("hello"), "TestStr");
    assert!(handle.get().is_ok());
    handle.close().unwrap();
    assert!(handle.get().is_err());
}

#[test]
fn test_handle_double_close() {
    let handle = PluginHandle::new(100_u64, "TestU64");
    handle.close().unwrap();
    assert!(handle.close().is_err());
}

#[test]
fn test_handle_release_idempotent() {
    let handle = PluginHandle::new(1_u32, "TestU32");
    handle.release();
    handle.release(); // should not panic
}

#[test]
fn test_handle_table_insert_and_get() {
    let mut table = HandleTable::new();
    let handle = PluginHandle::new(String::from("test"), "Str");
    let id = table.insert(handle.clone_inner());
    assert!(id >= 1);

    let retrieved: Option<_> = table.get::<String>(id);
    assert!(retrieved.is_some());
}

#[test]
fn test_handle_table_remove() {
    let mut table = HandleTable::new();
    let handle = PluginHandle::new(42_i32, "Int");
    let id = table.insert(handle.clone_inner());
    assert!(table.remove(id));
    assert!(table.get::<i32>(id).is_none());
}

#[test]
fn test_handle_table_sequential_ids() {
    let mut table = HandleTable::new();
    let h1 = PluginHandle::new(1_u8, "U8");
    let h2 = PluginHandle::new(2_u8, "U8");
    let id1 = table.insert(h1.clone_inner());
    let id2 = table.insert(h2.clone_inner());
    assert_eq!(id2, id1 + 1);
}
