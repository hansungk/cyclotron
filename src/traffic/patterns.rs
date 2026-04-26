use crate::traffic::config::{TrafficConfig, TrafficPatternSpec};
use std::collections::HashMap;

#[derive(Debug, Clone)]
struct JavaRandom {
    seed: u64,
}

impl JavaRandom {
    const MULT: u64 = 0x5DEECE66D;
    const ADD: u64 = 0xB;
    const MASK: u64 = (1u64 << 48) - 1;

    fn new(seed: i64) -> Self {
        Self {
            seed: (seed as u64 ^ Self::MULT) & Self::MASK,
        }
    }

    fn next(&mut self, bits: u32) -> i32 {
        self.seed = (self.seed.wrapping_mul(Self::MULT).wrapping_add(Self::ADD)) & Self::MASK;
        (self.seed >> (48 - bits)) as i32
    }

    fn next_int(&mut self, bound: i32) -> i32 {
        assert!(bound > 0, "bound must be positive");
        // Power-of-2 fast path
        if bound & (bound - 1) == 0 {
            return (((bound as i64) * (self.next(31) as i64)) >> 31) as i32;
        }
        loop {
            let bits = self.next(31);
            let val = bits % bound;
            if bits - val + (bound - 1) >= 0 {
                return val;
            }
        }
    }

    fn between(&mut self, low: i32, high: i32) -> i32 {
        low + self.next_int(high - low)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternOp {
    Read,
    Write,
}

impl PatternOp {
    pub fn is_store(self) -> bool {
        matches!(self, Self::Write)
    }

    fn short(self) -> &'static str {
        match self {
            Self::Read => "r",
            Self::Write => "w",
        }
    }
}

#[derive(Debug, Clone)]
enum PatternKind {
    Strided {
        warp_stride: u64,
        lane_stride: u64,
    },
    Tiled {
        tile_m: u64,
        tile_n: u64,
        transpose: bool,
    },
    Swizzled {
        tile_size: u64,
        transpose: bool,
    },
    Random {
        min: u64,
        max: u64,
        seed: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RandomStreamKey {
    min: u64,
    max: u64,
    seed: u64,
    req_bytes: u32,
}

#[derive(Debug, Clone)]
pub struct CompiledPattern {
    pub suite: String,
    pub name: String,
    pub op: PatternOp,
    pub req_bytes: u32,
    pub reqs_per_pattern: u32,
    pub active_lanes: usize,
    pub max_inflight_per_lane: usize,
    pub issue_gap: u64,
    base_offset_bytes: u64,
    within_bytes: u64,
    kind: PatternKind,
}

impl CompiledPattern {
    fn offset_bytes(&self, req_idx: u32, lane_idx: usize, lanes: usize) -> u64 {
        let req_bytes = self.req_bytes.max(1) as u64;
        let lane = lane_idx as u64;
        let lanes_u64 = lanes.max(1) as u64;
        match self.kind {
            PatternKind::Strided {
                warp_stride,
                lane_stride,
            } => ((req_idx as u64 * warp_stride) * lanes_u64 + lane) * lane_stride * req_bytes,
            PatternKind::Tiled {
                tile_m,
                tile_n,
                transpose,
            } => {
                let tile_m = tile_m.max(1);
                let tile_n = tile_n.max(1);
                let tile_elems = tile_m.saturating_mul(tile_n).max(1);
                let elem_idx = req_idx as u64 * lanes_u64 + lane;
                let tile_idx = elem_idx / tile_elems;
                let idx_in_tile = elem_idx % tile_elems;
                let mut row = idx_in_tile / tile_n;
                let mut col = idx_in_tile % tile_n;
                if transpose {
                    std::mem::swap(&mut row, &mut col);
                }
                (tile_idx * tile_elems + row * tile_n + col) * req_bytes
            }
            PatternKind::Swizzled {
                tile_size,
                transpose,
            } => {
                let tile_size = tile_size.max(1);
                let tile_elems = tile_size.saturating_mul(tile_size).max(1);
                let elem_idx = req_idx as u64 * lanes_u64 + lane;
                let tile_idx = elem_idx / tile_elems;
                let idx_in_tile = elem_idx % tile_elems;
                let mut row = idx_in_tile / tile_size;
                let mut col = idx_in_tile % tile_size;
                if transpose {
                    std::mem::swap(&mut row, &mut col);
                }
                let rotated_col = (col + row) % tile_size;
                (tile_idx * tile_elems + row * tile_size + rotated_col) * req_bytes
            }
            PatternKind::Random { min, max, seed } => {
                if max <= min {
                    return min.saturating_mul(req_bytes);
                }
                // Fallback path for out-of-range indexed requests (normal path uses
                // precomputed random tables in PatternEngine for Radiance parity).
                let key = seed
                    ^ ((lane_idx as u64) << 32)
                    ^ (req_idx as u64)
                    ^ ((self.req_bytes as u64) << 48);
                let span = max - min;
                let sample = min + (mix64(key) % span);
                sample.saturating_mul(req_bytes)
            }
        }
    }

    fn random_stream_key(&self) -> Option<RandomStreamKey> {
        match self.kind {
            PatternKind::Random { min, max, seed } => Some(RandomStreamKey {
                min,
                max,
                seed,
                req_bytes: self.req_bytes.max(1),
            }),
            _ => None,
        }
    }

    pub fn lane_addr(&self, req_idx: u32, lane_idx: usize, lanes: usize, smem_base: u64) -> u64 {
        let offset = self.offset_bytes(req_idx, lane_idx, lanes);
        let within = self.within_bytes.max(self.req_bytes.max(1) as u64);
        smem_base.saturating_add(offset % within)
    }
}

#[derive(Debug, Clone, Default)]
pub struct PatternEngine {
    patterns: Vec<CompiledPattern>,
    lanes: usize,
    default_base: u64,
    random_tables: Vec<Option<Vec<u64>>>, // flattened [lane * reqs_per_pattern + t]
}

impl PatternEngine {
    pub fn new(config: &TrafficConfig) -> Self {
        let lanes = config.num_lanes.max(1);
        let default_base = config.address.smem_base;
        let patterns: Vec<CompiledPattern> = config
            .patterns
            .iter()
            .enumerate()
            .map(|(idx, spec)| compile_pattern(spec, idx, config))
            .collect();
        let random_tables = precompute_random_tables(&patterns, lanes);
        Self {
            patterns,
            lanes,
            default_base,
            random_tables,
        }
    }

    pub fn len(&self) -> usize {
        self.patterns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    pub fn pattern_name(&self, idx: usize) -> Option<&str> {
        self.patterns.get(idx).map(|p| p.name.as_str())
    }

    pub fn pattern(&self, idx: usize) -> Option<&CompiledPattern> {
        self.patterns.get(idx)
    }

    pub fn lane_addr(&self, pattern_idx: usize, req_idx: u32, lane_idx: usize) -> Option<u64> {
        self.lane_addr_with_base(pattern_idx, req_idx, lane_idx, self.default_base)
    }

    pub fn lane_addr_with_base(
        &self,
        pattern_idx: usize,
        req_idx: u32,
        lane_idx: usize,
        base: u64,
    ) -> Option<u64> {
        let pattern = self.patterns.get(pattern_idx)?;
        let offset = self
            .random_offset(pattern_idx, req_idx, lane_idx)
            .unwrap_or_else(|| pattern.offset_bytes(req_idx, lane_idx, self.lanes));
        let within = pattern.within_bytes.max(pattern.req_bytes.max(1) as u64);
        Some(
            base.saturating_add(pattern.base_offset_bytes)
                .saturating_add(offset % within),
        )
    }

    fn random_offset(&self, pattern_idx: usize, req_idx: u32, lane_idx: usize) -> Option<u64> {
        let table = self.random_tables.get(pattern_idx)?.as_ref()?;
        if lane_idx >= self.lanes {
            return None;
        }
        let pattern = self.patterns.get(pattern_idx)?;
        let reqs_per_pattern = pattern.reqs_per_pattern.max(1) as usize;
        let idx = lane_idx
            .checked_mul(reqs_per_pattern)?
            .checked_add(req_idx as usize)?;
        table.get(idx).copied()
    }
}

fn precompute_random_tables(patterns: &[CompiledPattern], lanes: usize) -> Vec<Option<Vec<u64>>> {
    let mut tables: Vec<Option<Vec<u64>>> = vec![None; patterns.len()];
    if patterns.is_empty() || lanes == 0 {
        return tables;
    }

    let mut streams: HashMap<RandomStreamKey, JavaRandom> = HashMap::new();
    for lane in 0..lanes {
        for (pattern_idx, pattern) in patterns.iter().enumerate() {
            let Some(key) = pattern.random_stream_key() else {
                continue;
            };
            let stream = streams
                .entry(key)
                .or_insert_with(|| JavaRandom::new(key.seed as i64));
            let reqs_per_pattern = pattern.reqs_per_pattern.max(1) as usize;
            let slot = tables[pattern_idx]
                .get_or_insert_with(|| vec![0; lanes.saturating_mul(reqs_per_pattern)]);
            let row_base = lane.saturating_mul(reqs_per_pattern);
            for t in 0..reqs_per_pattern {
                let sample = if key.max <= key.min {
                    key.min
                } else {
                    stream.between(key.min as i32, key.max as i32) as u64
                };
                slot[row_base + t] = sample.saturating_mul(pattern.req_bytes.max(1) as u64);
            }
        }
    }

    tables
}

fn compile_pattern(
    spec: &TrafficPatternSpec,
    index: usize,
    config: &TrafficConfig,
) -> CompiledPattern {
    let kind_key = spec.kind.trim().to_ascii_lowercase();
    let req_bytes = spec.req_bytes.max(1);
    let reqs_per_pattern = spec
        .reqs_per_pattern
        .unwrap_or(config.reqs_per_pattern)
        .max(1);
    let active_lanes = spec
        .active_lanes
        .unwrap_or(config.num_lanes)
        .max(1)
        .min(config.num_lanes.max(1));
    let max_inflight_per_lane = spec
        .max_inflight_per_lane
        .unwrap_or(config.issue.max_inflight_per_lane)
        .max(1);
    let op = parse_op(&spec.op);
    let within_default = config.address.smem_size_bytes.max(req_bytes as u64);
    let within_bytes = spec
        .within_bytes
        .unwrap_or(within_default)
        .max(req_bytes as u64);

    let kind = match kind_key.as_str() {
        "strided" => PatternKind::Strided {
            warp_stride: spec.warp_stride.max(1) as u64,
            lane_stride: spec.lane_stride as u64,
        },
        "tiled" => PatternKind::Tiled {
            tile_m: spec.tile_m.max(1) as u64,
            tile_n: spec.tile_n.max(1) as u64,
            transpose: spec.transpose,
        },
        "swizzled" => PatternKind::Swizzled {
            tile_size: spec.tile_size.max(1) as u64,
            transpose: spec.transpose,
        },
        "random" | "random_access" => {
            let min = spec.random_min as u64;
            let max = if spec.random_max == 0 {
                (within_bytes / req_bytes as u64).max(min + 1)
            } else {
                spec.random_max as u64
            };
            PatternKind::Random {
                min,
                max: max.max(min + 1),
                seed: spec.seed,
            }
        }
        other => panic!(
            "unsupported traffic pattern kind '{}' at index {} (expected strided|tiled|swizzled|random)",
            other, index
        ),
    };

    let name = if spec.name.is_empty() {
        default_pattern_name(spec, &kind, req_bytes, op)
    } else {
        spec.name.clone()
    };

    CompiledPattern {
        suite: if spec.suite.is_empty() {
            "traffic_frontend".to_string()
        } else {
            spec.suite.clone()
        },
        name,
        op,
        req_bytes,
        reqs_per_pattern,
        active_lanes,
        max_inflight_per_lane,
        issue_gap: spec.issue_gap,
        base_offset_bytes: spec.base_offset_bytes as u64,
        within_bytes,
        kind,
    }
}

fn parse_op(op: &str) -> PatternOp {
    match op.trim().to_ascii_lowercase().as_str() {
        "read" | "r" | "get" => PatternOp::Read,
        "write" | "w" | "put" | "store" => PatternOp::Write,
        other => panic!("unsupported traffic op '{}'; expected read/write", other),
    }
}

fn default_pattern_name(
    spec: &TrafficPatternSpec,
    kind: &PatternKind,
    req_bytes: u32,
    op: PatternOp,
) -> String {
    let base = match kind {
        PatternKind::Strided {
            warp_stride,
            lane_stride,
        } => format!("strided({}, {})@{}", warp_stride, lane_stride, req_bytes),
        PatternKind::Tiled {
            tile_m,
            tile_n,
            transpose,
        } => {
            let suffix = if *transpose { ".T" } else { "" };
            format!("tiled({}, {})@{}{}", tile_m, tile_n, req_bytes, suffix)
        }
        PatternKind::Swizzled {
            tile_size,
            transpose,
        } => {
            let suffix = if *transpose { ".T" } else { "" };
            format!("swizzled({})@{}{}", tile_size, req_bytes, suffix)
        }
        PatternKind::Random { seed, .. } => format!("random({})", seed),
    };
    let _ = spec;
    format!("{}_{}", base, op.short())
}

fn mix64(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traffic::config::{
        TrafficAddressConfig, TrafficConfig, TrafficIssueConfig, TrafficLoggingConfig,
    };

    fn base_cfg(patterns: Vec<TrafficPatternSpec>, lanes: usize, reqs: u32) -> TrafficConfig {
        TrafficConfig {
            enabled: true,
            file: None,
            gmem_entry_mode: crate::traffic::config::GmemEntryMode::HierarchyOnly,
            lockstep_patterns: true,
            reqs_per_pattern: reqs,
            num_lanes: lanes,
            address: TrafficAddressConfig {
                cluster_id: 0,
                smem_base: 0x4000_0000,
                smem_size_bytes: 128 << 10,
                gmem_base: 0x0100_0000,
                gmem_size_bytes: 1 << 20,
            },
            issue: TrafficIssueConfig::default(),
            logging: TrafficLoggingConfig::default(),
            patterns,
        }
    }

    fn spec(kind: &str, op: &str) -> TrafficPatternSpec {
        TrafficPatternSpec {
            kind: kind.to_string(),
            op: op.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn strided_formula_matches_radiance_definition() {
        let mut p = spec("strided", "read");
        p.req_bytes = 4;
        p.warp_stride = 2;
        p.lane_stride = 8;
        let cfg = base_cfg(vec![p], 16, 8);
        let engine = PatternEngine::new(&cfg);

        let lane0_t0 = engine.lane_addr(0, 0, 0).unwrap();
        let lane3_t2 = engine.lane_addr(0, 2, 3).unwrap();
        assert_eq!(lane0_t0, 0x4000_0000);
        // ((2*2)*16 + 3) * 8 * 4 = 2144
        assert_eq!(lane3_t2, 0x4000_0000 + 2144);
    }

    #[test]
    fn random_stream_is_deterministic_and_bounded() {
        let mut p0 = spec("random", "write");
        p0.name = "random(0)_w".to_string();
        p0.seed = 0;
        p0.req_bytes = 4;
        p0.random_min = 0;
        p0.random_max = 16;

        let mut p1 = spec("random", "read");
        p1.name = "random(0)_r".to_string();
        p1.seed = 0;
        p1.req_bytes = 4;
        p1.random_min = 0;
        p1.random_max = 16;

        let cfg = base_cfg(vec![p0, p1], 2, 3);
        let engine_a = PatternEngine::new(&cfg);
        let engine_b = PatternEngine::new(&cfg);

        for lane in 0..2 {
            for t in 0..3 {
                let a0 = engine_a.lane_addr(0, t, lane).unwrap();
                let b0 = engine_b.lane_addr(0, t, lane).unwrap();
                let a1 = engine_a.lane_addr(1, t, lane).unwrap();
                let b1 = engine_b.lane_addr(1, t, lane).unwrap();
                assert_eq!(a0, b0);
                assert_eq!(a1, b1);
                assert!((0x4000_0000..0x4000_0000 + 64).contains(&a0));
                assert!((0x4000_0000..0x4000_0000 + 64).contains(&a1));
            }
        }
    }

    #[test]
    fn pattern_issue_overrides_use_per_pattern_values() {
        let mut p = spec("strided", "read");
        p.reqs_per_pattern = Some(7);
        p.max_inflight_per_lane = Some(3);
        p.issue_gap = 2;

        let cfg = base_cfg(vec![p], 16, 4096);
        let engine = PatternEngine::new(&cfg);
        let pattern = engine.pattern(0).unwrap();

        assert_eq!(pattern.reqs_per_pattern, 7);
        assert_eq!(pattern.max_inflight_per_lane, 3);
        assert_eq!(pattern.issue_gap, 2);
    }
}
