use crate::timeflow::gmem::cache::CacheTagArray;

#[test]
fn cache_tag_array_hits_and_evicts() {
    let mut tags = CacheTagArray::new(1, 2);
    assert!(!tags.probe(0));
    tags.fill(0);
    tags.fill(1);
    assert!(tags.probe(0));
    tags.fill(2);
    assert!(tags.probe(0));
    assert!(!tags.probe(1));
}

#[test]
fn probe_returns_false_for_empty_cache() {
    let mut tags = CacheTagArray::new(4, 2);
    assert!(!tags.probe(123));
}

#[test]
fn fill_then_probe_returns_true() {
    let mut tags = CacheTagArray::new(4, 2);
    tags.fill(42);
    assert!(tags.probe(42));
}

#[test]
fn invalidate_all_clears_entire_cache() {
    let mut tags = CacheTagArray::new(4, 2);
    tags.fill(1);
    tags.fill(2);
    tags.invalidate_all();
    assert!(!tags.probe(1));
    assert!(!tags.probe(2));
}

#[test]
fn single_set_single_way_cache() {
    let mut tags = CacheTagArray::new(1, 1);
    tags.fill(1);
    assert!(tags.probe(1));
    tags.fill(2);
    assert!(!tags.probe(1));
    assert!(tags.probe(2));
}
