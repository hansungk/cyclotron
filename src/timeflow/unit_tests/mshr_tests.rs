use crate::timeflow::gmem::mshr::{MissMetadata, MshrTable};
use crate::timeflow::GmemRequest;

#[test]
fn mshr_table_merges_requests() {
    let mut table = MshrTable::new(1);
    let req = GmemRequest::new(0, 16, 0xF, true);
    let meta = MissMetadata::from_request(&req);
    assert!(table.ensure_entry(1, meta).unwrap());
    assert!(table.ensure_entry(2, meta).is_err());
    table.set_ready_at(1, 10);
    let merged = GmemRequest::new(1, 8, 0xF, true);
    let ready_at = table.merge_request(1, merged).unwrap();
    assert_eq!(ready_at, 10);
    let entry = table.remove_entry(1).unwrap();
    assert_eq!(entry.merged.len(), 1);
}

#[test]
fn new_mshr_table_is_empty() {
    let table = MshrTable::new(4);
    assert!(!table.has_entry(0));
    assert!(!table.has_entry(123));
}

#[test]
fn ensure_entry_existing_returns_false() {
    let mut table = MshrTable::new(2);
    let req = GmemRequest::new(0, 16, 0xF, true);
    let meta = MissMetadata::from_request(&req);
    assert!(table.ensure_entry(7, meta).unwrap());
    assert!(!table.ensure_entry(7, meta).unwrap());
}

#[test]
fn merge_request_to_nonexistent_returns_none() {
    let mut table = MshrTable::new(1);
    let req = GmemRequest::new(0, 8, 0xF, true);
    assert!(table.merge_request(1, req).is_none());
}

#[test]
fn remove_entry_frees_slot() {
    let mut table = MshrTable::new(1);
    let req = GmemRequest::new(0, 16, 0xF, true);
    let meta = MissMetadata::from_request(&req);
    assert!(table.ensure_entry(1, meta).unwrap());
    assert!(!table.can_allocate(2));
    table.remove_entry(1).unwrap();
    assert!(table.can_allocate(2));
}

#[test]
fn can_allocate_true_when_under_capacity() {
    let mut table = MshrTable::new(2);
    let req = GmemRequest::new(0, 16, 0xF, true);
    let meta = MissMetadata::from_request(&req);
    assert!(table.can_allocate(1));
    assert!(table.ensure_entry(1, meta).unwrap());
    assert!(table.can_allocate(2));
}

#[test]
fn can_allocate_false_when_full_different_line() {
    let mut table = MshrTable::new(1);
    let req = GmemRequest::new(0, 16, 0xF, true);
    let meta = MissMetadata::from_request(&req);
    assert!(table.ensure_entry(1, meta).unwrap());
    assert!(!table.can_allocate(2));
}

#[test]
fn set_ready_at_updates_entry() {
    let mut table = MshrTable::new(1);
    let req = GmemRequest::new(0, 16, 0xF, true);
    let meta = MissMetadata::from_request(&req);
    table.ensure_entry(1, meta).unwrap();
    table.set_ready_at(1, 42);
    let merged = GmemRequest::new(0, 8, 0xF, true);
    let ready_at = table.merge_request(1, merged).unwrap();
    assert_eq!(42, ready_at);
}

#[test]
fn capacity_of_one() {
    let mut table = MshrTable::new(1);
    let req = GmemRequest::new(0, 16, 0xF, true);
    let meta = MissMetadata::from_request(&req);
    assert!(table.ensure_entry(1, meta).unwrap());
    assert!(!table.can_allocate(2));
}

#[test]
fn multiple_merges_same_entry() {
    let mut table = MshrTable::new(1);
    let req = GmemRequest::new(0, 16, 0xF, true);
    let meta = MissMetadata::from_request(&req);
    table.ensure_entry(1, meta).unwrap();
    for _ in 0..10 {
        let merged = GmemRequest::new(0, 8, 0xF, true);
        assert!(table.merge_request(1, merged).is_none());
    }
    let entry = table.remove_entry(1).unwrap();
    assert_eq!(entry.merged.len(), 10);
}

#[test]
fn fill_and_drain_repeatedly() {
    let mut table = MshrTable::new(4);
    let req = GmemRequest::new(0, 16, 0xF, true);
    let meta = MissMetadata::from_request(&req);
    for round in 0..100 {
        for line in 0..4 {
            assert!(table.ensure_entry(line, meta).unwrap(), "round {round}");
        }
        for line in 0..4 {
            assert!(table.remove_entry(line).is_some(), "round {round}");
        }
    }
}
