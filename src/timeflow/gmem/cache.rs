#[derive(Debug)]
pub(crate) struct CacheTagArray {
    sets: usize,
    ways: usize,
    tags: Vec<Option<u64>>,
    lru: Vec<Vec<usize>>,
}

impl CacheTagArray {
    fn build_lru(sets: usize, ways: usize) -> Vec<Vec<usize>> {
        let mut lru = Vec::with_capacity(sets);
        for _ in 0..sets {
            lru.push((0..ways).collect());
        }
        lru
    }
    pub(crate) fn new(sets: usize, ways: usize) -> Self {
        let sets = sets.max(1);
        let ways = ways.max(1);
        let tags = vec![None; sets * ways];
        let lru = Self::build_lru(sets, ways);
        Self {
            sets,
            ways,
            tags,
            lru,
        }
    }

    fn reset_lru_for_set(&mut self, set_idx: usize) {
        self.lru[set_idx].clear();
        self.lru[set_idx].extend(0..self.ways);
    }

    fn idx(&self, set_idx: usize, way: usize) -> usize {
        set_idx * self.ways + way
    }

    fn bounds_ok(&self, set_idx: usize, way: usize) -> bool {
        set_idx < self.sets && way < self.ways
    }

    pub(crate) fn get_tag(&self, set_idx: usize, way: usize) -> Option<u64> {
        debug_assert!(self.bounds_ok(set_idx, way));
        let idx = self.idx(set_idx, way);
        self.tags[idx]
    }

    pub(crate) fn set_tag(&mut self, set_idx: usize, way: usize, val: Option<u64>) {
        debug_assert!(self.bounds_ok(set_idx, way));
        let idx = self.idx(set_idx, way);
        self.tags[idx] = val;
    }

    pub(crate) fn probe(&mut self, line_addr: u64) -> bool {
        let set_idx = (line_addr as usize) % self.sets;
        let mut hit_way = None;
        for way in 0..self.ways {
            let tag = self.get_tag(set_idx, way);
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
            let tag = self.get_tag(set_idx, way);
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
            if self.get_tag(set_idx, way).is_none() {
                empty_way = Some(way);
                break;
            }
        }
        let way = empty_way.unwrap_or_else(|| *self.lru[set_idx].last().unwrap_or(&0));
        self.set_tag(set_idx, way, Some(line_addr));
        self.touch(set_idx, way);
    }

    pub(crate) fn invalidate_all(&mut self) {
        for set_idx in 0..self.sets {
            for way in 0..self.ways {
                self.set_tag(set_idx, way, None);
            }
            self.reset_lru_for_set(set_idx);
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

