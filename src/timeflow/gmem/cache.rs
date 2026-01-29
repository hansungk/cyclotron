#[derive(Debug)]
pub(crate) struct CacheTagArray {
    sets: usize,
    ways: usize,
    tags: Vec<Option<u64>>,
    lru: Vec<Vec<usize>>,
}

impl CacheTagArray {
    pub(crate) fn new(sets: usize, ways: usize) -> Self {
        let sets = sets.max(1);
        let ways = ways.max(1);
        let tags = vec![None; sets * ways];
        let mut lru = Vec::with_capacity(sets);
        for _ in 0..sets {
            lru.push((0..ways).collect());
        }
        Self {
            sets,
            ways,
            tags,
            lru,
        }
    }

    fn idx(&self, set_idx: usize, way: usize) -> usize {
        set_idx * self.ways + way
    }

    pub(crate) fn probe(&mut self, line_addr: u64) -> bool {
        let set_idx = (line_addr as usize) % self.sets;
        let mut hit_way = None;
        for way in 0..self.ways {
            let tag = self.tags[self.idx(set_idx, way)];
            if tag == Some(line_addr) {
                hit_way = Some(way);
                break;
            }
        }
        if let Some(way) = hit_way {
            self.touch(set_idx, way);
            return true;
        }
        false
    }

    pub(crate) fn fill(&mut self, line_addr: u64) {
        let set_idx = (line_addr as usize) % self.sets;
        let mut hit_way = None;
        for way in 0..self.ways {
            let tag = self.tags[self.idx(set_idx, way)];
            if tag == Some(line_addr) {
                hit_way = Some(way);
                break;
            }
        }
        if let Some(way) = hit_way {
            self.touch(set_idx, way);
            return;
        }

        let mut empty_way = None;
        for way in 0..self.ways {
            if self.tags[self.idx(set_idx, way)].is_none() {
                empty_way = Some(way);
                break;
            }
        }
        let way = empty_way.unwrap_or_else(|| *self.lru[set_idx].last().unwrap_or(&0));
        let idx = self.idx(set_idx, way);
        self.tags[idx] = Some(line_addr);
        self.touch(set_idx, way);
    }

    pub(crate) fn invalidate_all(&mut self) {
        for set_idx in 0..self.sets {
            for way in 0..self.ways {
                let idx = self.idx(set_idx, way);
                self.tags[idx] = None;
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
