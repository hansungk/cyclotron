use crate::traffic::config::{TrafficConfig, TrafficPatternSpec};

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

#[derive(Debug, Clone)]
pub struct CompiledPattern {
    pub name: String,
    pub op: PatternOp,
    pub req_bytes: u32,
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
                let span = max - min;
                let key = seed
                    ^ ((lane_idx as u64) << 32)
                    ^ (req_idx as u64)
                    ^ ((self.req_bytes as u64) << 48);
                let sample = min + (mix64(key) % span);
                sample.saturating_mul(req_bytes)
            }
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
    smem_base: u64,
}

impl PatternEngine {
    pub fn new(config: &TrafficConfig) -> Self {
        let lanes = config.num_lanes.max(1);
        let smem_base = config.address.smem_base;
        let patterns = config
            .patterns
            .iter()
            .enumerate()
            .map(|(idx, spec)| compile_pattern(spec, idx, config))
            .collect();
        Self {
            patterns,
            lanes,
            smem_base,
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
        self.patterns
            .get(pattern_idx)
            .map(|p| p.lane_addr(req_idx, lane_idx, self.lanes, self.smem_base))
    }
}

fn compile_pattern(
    spec: &TrafficPatternSpec,
    index: usize,
    config: &TrafficConfig,
) -> CompiledPattern {
    let kind_key = spec.kind.trim().to_ascii_lowercase();
    let req_bytes = spec.req_bytes.max(1);
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
        name,
        op,
        req_bytes,
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
