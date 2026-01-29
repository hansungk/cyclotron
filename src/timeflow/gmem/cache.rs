#[derive(Debug)]
pub(crate) struct CacheTagArray {
    sets: usize,
    ways: usize,
    tags: Vec<Vec<Option<u64>>>,
    lru: Vec<Vec<usize>>,
}

impl CacheTagArray {
    pub(crate) fn new(sets: usize, ways: usize) -> Self {
        let sets = sets.max(1);
        let ways = ways.max(1);
        let mut tags = Vec::with_capacity(sets);
        let mut lru = Vec::with_capacity(sets);
        for _ in 0..sets {
            tags.push(vec![None; ways]);
            lru.push((0..ways).collect());
        }
        Self {
            sets,
            ways,
            tags,
            lru,
        }
    }

    pub(crate) fn probe(&mut self, line_addr: u64) -> bool {
        let set_idx = (line_addr as usize) % self.sets;
        let set = &mut self.tags[set_idx];
        if let Some(way) = set.iter().position(|tag| tag.map_or(false, |t| t == line_addr)) {
            self.touch(set_idx, way);
            return true;
        }
        false
    }

    pub(crate) fn fill(&mut self, line_addr: u64) {
        let set_idx = (line_addr as usize) % self.sets;
        let set = &mut self.tags[set_idx];
        if let Some(way) = set.iter().position(|tag| tag.map_or(false, |t| t == line_addr)) {
            self.touch(set_idx, way);
            return;
        }

        let way = if let Some(idx) = set.iter().position(|tag| tag.is_none()) {
            idx
        } else {
            *self.lru[set_idx].last().unwrap_or(&0)
        };
        set[way] = Some(line_addr);
        self.touch(set_idx, way);
    }

    pub(crate) fn invalidate_all(&mut self) {
        for set_idx in 0..self.sets {
            for way in 0..self.ways {
                self.tags[set_idx][way] = None;
            }
            self.lru[set_idx].clear();
            self.lru[set_idx].extend(0..self.ways);
        }
    }

    fn touch(&mut self, set_idx: usize, way: usize) {
        let order = &mut self.lru[set_idx];
        if let Some(pos) = order.iter().position(|&idx| idx == way) {
            order.remove(pos);
        }
        order.insert(0, way);
    }
}

#[cfg(test)]
mod tests {
    use super::CacheTagArray;

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
}
