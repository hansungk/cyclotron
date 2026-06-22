use std::collections::HashMap;
use std::os::raw::c_int;
use std::slice;

const ADDRESS_SPACE_GLOBAL_MEMORY: u32 = 0;
const ADDRESS_SPACE_SHARED_MEMORY: u32 = 1;
const MEM_OP_LOAD_BYTE: u32 = 0;
const MEM_OP_LOAD_BYTE_UNSIGNED: u32 = 1;
const MEM_OP_LOAD_HALF: u32 = 2;
const MEM_OP_LOAD_HALF_UNSIGNED: u32 = 3;
const MEM_OP_LOAD_WORD: u32 = 4;
const MEM_OP_STORE_BYTE: u32 = 5;
const MEM_OP_STORE_HALF: u32 = 6;
const MEM_OP_STORE_WORD: u32 = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemSpace {
    Global,
    Shared,
}

impl MemSpace {
    fn from_address_space(address_space: u32) -> Option<Self> {
        match address_space {
            ADDRESS_SPACE_GLOBAL_MEMORY => Some(Self::Global),
            ADDRESS_SPACE_SHARED_MEMORY => Some(Self::Shared),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Shared => "shared",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MemInflight {
    token: u64,
    op: u32,
    warp_id: u32,
    dest_reg: u32,
    tmask: u64,
    byte_offsets: Vec<u8>,
    debug_id: u32,
}

impl MemInflight {
    fn is_load(&self) -> bool {
        is_load_op(self.op)
    }
}

#[derive(Default)]
struct MemPortState {
    inflight: HashMap<u64, MemInflight>,
    store_inflight: HashMap<u64, ()>,
    retired_tags: HashMap<u64, ()>,
}

impl MemPortState {
    fn reset(&mut self) {
        self.inflight.clear();
        self.store_inflight.clear();
        self.retired_tags.clear();
    }

    fn queues_empty(&self, pending_request: bool) -> bool {
        self.inflight.is_empty() && self.store_inflight.is_empty() && !pending_request
    }

    fn source_busy(&self, tag: u64) -> bool {
        self.inflight.contains_key(&tag) || self.store_inflight.contains_key(&tag)
    }
}

struct MemRequestOutputs {
    valid: *mut u8,
    tag: *mut u32,
    op: *mut u32,
    address: *mut u32,
    data: *mut u32,
    mask: *mut u32,
    tmask: *mut u32,
}

struct MemResponseInputs {
    valid: u8,
    tag: *const u32,
    lane_valid: *const u32,
    data: *const u32,
    ready: *mut u8,
}

pub(super) struct CyclotronLsuModel {
    arch_len: usize,
    num_warps: usize,
    num_lsu_lanes: usize,
    warp_id_bits: usize,
    token_bits: usize,
    address_space_bits: usize,
    mem_op_bits: usize,
    preg_bits: usize,
    queue_index_bits: usize,
    packet_bits: usize,
    source_id_bits: usize,
    per_lane_mask_bits: usize,
    debug_id_port_bits: usize,
    reservation_counters: Vec<u32>,
    reservation_debug_ids: HashMap<u64, u32>,
    global: MemPortState,
    shared: MemPortState,
    core_reservations_req_ready_words: usize,
    core_reservations_resp_valid_words: usize,
    core_reservations_resp_bits_token_words: usize,
    core_resp_bits_warp_id_words: usize,
    core_resp_bits_packet_words: usize,
    core_resp_bits_tmask_words: usize,
    core_resp_bits_dest_reg_words: usize,
    core_resp_bits_writeback_data_words: usize,
    core_resp_bits_debug_id_words: usize,
    mem_req_bits_tag_words: usize,
    mem_req_bits_op_words: usize,
    mem_req_bits_address_words: usize,
    mem_req_bits_data_words: usize,
    mem_req_bits_mask_words: usize,
    mem_req_bits_tmask_words: usize,
}

impl CyclotronLsuModel {
    fn new(
        arch_len: c_int,
        num_warps: c_int,
        num_lanes: c_int,
        num_lsu_lanes: c_int,
        warp_id_bits: c_int,
        token_bits: c_int,
        address_space_bits: c_int,
        mem_op_bits: c_int,
        preg_bits: c_int,
        packet_bits: c_int,
        source_id_bits: c_int,
        per_lane_mask_bits: c_int,
        debug_id_port_bits: c_int,
    ) -> Self {
        let arch_len = arch_len as usize;
        let num_warps = num_warps as usize;
        let num_lanes = num_lanes as usize;
        let num_lsu_lanes = num_lsu_lanes as usize;
        let token_bits = token_bits as usize;
        let warp_id_bits = warp_id_bits as usize;
        let address_space_bits = address_space_bits as usize;
        let mem_op_bits = mem_op_bits as usize;
        let packet_bits = packet_bits as usize;
        let source_id_bits = source_id_bits as usize;
        let per_lane_mask_bits = per_lane_mask_bits as usize;
        assert_eq!(
            num_lsu_lanes, num_lanes,
            "cyclotron LSU model currently assumes num_lsu_lanes == num_lanes"
        );
        assert!(
            arch_len <= 32,
            "cyclotron LSU model currently supports arch_len <= 32, got {}",
            arch_len
        );
        assert!(
            num_lanes <= 64,
            "cyclotron LSU model currently supports <= 64 lanes for masks, got {}",
            num_lanes
        );
        let queue_index_bits = token_bits
            .checked_sub(warp_id_bits + address_space_bits + 1)
            .expect("cyclotron LSU token is too narrow for its fields");
        assert!(
            token_bits <= 64,
            "cyclotron LSU token width {} exceeds Rust u64 packing",
            token_bits
        );
        assert!(
            address_space_bits <= 32 && mem_op_bits <= 32,
            "cyclotron LSU reservation fields exceed Rust u32 unpacking"
        );
        assert!(
            preg_bits <= 32 && debug_id_port_bits <= 32,
            "cyclotron LSU response metadata fields exceed Rust u32 unpacking"
        );
        assert!(
            queue_index_bits <= 32,
            "cyclotron LSU queue index width {} exceeds Rust u32 counters",
            queue_index_bits
        );
        assert!(
            source_id_bits <= 64,
            "cyclotron LSU source id width {} exceeds Rust u64 packing",
            source_id_bits
        );
        assert!(
            source_id_bits == token_bits + packet_bits,
            "cyclotron LSU source id width {} does not match token_bits {} + packet_bits {}",
            source_id_bits,
            token_bits,
            packet_bits
        );

        Self {
            arch_len,
            num_warps,
            num_lsu_lanes,
            warp_id_bits,
            token_bits,
            address_space_bits,
            mem_op_bits,
            preg_bits: preg_bits as usize,
            queue_index_bits,
            packet_bits,
            source_id_bits,
            per_lane_mask_bits,
            debug_id_port_bits: debug_id_port_bits as usize,
            reservation_counters: vec![0; num_warps * 4],
            reservation_debug_ids: HashMap::new(),
            global: MemPortState::default(),
            shared: MemPortState::default(),
            core_reservations_req_ready_words: dpi_words(num_warps),
            core_reservations_resp_valid_words: dpi_words(num_warps),
            core_reservations_resp_bits_token_words: dpi_words(num_warps * token_bits),
            core_resp_bits_warp_id_words: dpi_words(warp_id_bits),
            core_resp_bits_packet_words: dpi_words(packet_bits as usize),
            core_resp_bits_tmask_words: dpi_words(num_lanes as usize),
            core_resp_bits_dest_reg_words: dpi_words(preg_bits as usize),
            core_resp_bits_writeback_data_words: dpi_words((num_lsu_lanes * arch_len) as usize),
            core_resp_bits_debug_id_words: dpi_words(debug_id_port_bits as usize),
            mem_req_bits_tag_words: dpi_words(source_id_bits as usize),
            mem_req_bits_op_words: dpi_words(mem_op_bits),
            mem_req_bits_address_words: dpi_words((num_lsu_lanes * arch_len) as usize),
            mem_req_bits_data_words: dpi_words((num_lsu_lanes * arch_len) as usize),
            mem_req_bits_mask_words: dpi_words((num_lsu_lanes * per_lane_mask_bits) as usize),
            mem_req_bits_tmask_words: dpi_words(num_lsu_lanes as usize),
        }
    }

    fn reset(&mut self) {
        self.reservation_counters.fill(0);
        self.reservation_debug_ids.clear();
        self.global.reset();
        self.shared.reset();
    }

    fn reservation_slot(&self, warp_id: usize, address_space: u32, op: u32) -> Option<usize> {
        let address_space = address_space as usize;
        if warp_id >= self.num_warps || address_space > 1 {
            return None;
        }

        let queue_kind = if is_load_op(op) {
            0
        } else if is_store_like_op(op) {
            1
        } else {
            return None;
        };

        Some(warp_id * 4 + address_space * 2 + queue_kind)
    }

    fn reservation_token(&self, warp_id: usize, address_space: u32, op: u32) -> Option<u64> {
        let slot = self.reservation_slot(warp_id, address_space, op)?;
        let ldq = is_load_op(op) as u64;
        let index = (self.reservation_counters[slot] & bit_mask(self.queue_index_bits)) as u64;

        let token = ((warp_id as u64) << (self.address_space_bits + 1 + self.queue_index_bits))
            | ((address_space as u64) << (1 + self.queue_index_bits))
            | (ldq << self.queue_index_bits)
            | index;

        Some(token)
    }

    fn commit_reservation(&mut self, warp_id: usize, address_space: u32, op: u32, debug_id: u32) {
        if let Some(token) = self.reservation_token(warp_id, address_space, op) {
            self.reservation_debug_ids.insert(token, debug_id);
            if let Some(slot) = self.reservation_slot(warp_id, address_space, op) {
                self.reservation_counters[slot] = self.reservation_counters[slot].wrapping_add(1);
            }
        }
    }

    fn token_warp_id(&self, token: u64) -> u32 {
        ((token >> (self.address_space_bits + 1 + self.queue_index_bits))
            & bit_mask_u64(self.warp_id_bits)) as u32
    }

    fn token_address_space(&self, token: u64) -> u32 {
        ((token >> (self.queue_index_bits + 1)) & bit_mask_u64(self.address_space_bits)) as u32
    }

    fn request_space(&self, token: u64, op: u32) -> Option<MemSpace> {
        if !is_supported_memory_op(op) {
            return None;
        }

        MemSpace::from_address_space(self.token_address_space(token))
    }

    fn mem_tag(&self, token: u64) -> u64 {
        token << self.packet_bits
    }

    fn packet_from_mem_tag(&self, tag: u64) -> u64 {
        tag & bit_mask_u64(self.packet_bits)
    }

    fn token_from_mem_tag(&self, tag: u64) -> u64 {
        tag >> self.packet_bits
    }

    fn base_mem_tag(&self, tag: u64) -> u64 {
        self.mem_tag(self.token_from_mem_tag(tag))
    }

    fn response_space(&self, tag: u64) -> Option<MemSpace> {
        MemSpace::from_address_space(self.token_address_space(self.token_from_mem_tag(tag)))
    }

    fn arch_mask(&self) -> u32 {
        bit_mask(self.arch_len)
    }

    fn word_mask(&self) -> u64 {
        bit_mask_u64(self.per_lane_mask_bits)
    }

    fn byte_offset(&self, address: u32) -> u8 {
        (address & (self.per_lane_mask_bits as u32 - 1)) as u8
    }

    fn lane_address(&self, base_address: u32, imm: u32) -> u32 {
        base_address.wrapping_add(imm) & self.arch_mask()
    }

    fn request_mask(&self, op: u32, address: u32) -> u64 {
        let mask = match op {
            MEM_OP_LOAD_BYTE | MEM_OP_LOAD_BYTE_UNSIGNED | MEM_OP_STORE_BYTE => {
                1u64 << self.byte_offset(address)
            }
            MEM_OP_LOAD_HALF | MEM_OP_LOAD_HALF_UNSIGNED | MEM_OP_STORE_HALF => {
                0b11u64 << self.byte_offset(address)
            }
            MEM_OP_LOAD_WORD | MEM_OP_STORE_WORD => self.word_mask(),
            _ => 0,
        };
        mask & self.word_mask()
    }

    fn shifted_store_data(&self, store_data: u32, address: u32) -> u32 {
        let shift = self.byte_offset(address) as u32 * 8;
        store_data.wrapping_shl(shift) & self.arch_mask()
    }

    fn load_writeback_data(&self, op: u32, word: u32, byte_offset: u8) -> u32 {
        let shift = byte_offset as u32 * 8;
        let result = match op {
            MEM_OP_LOAD_BYTE => sign_extend(word.wrapping_shr(shift) & 0xff, 8, self.arch_len),
            MEM_OP_LOAD_BYTE_UNSIGNED => word.wrapping_shr(shift) & 0xff,
            MEM_OP_LOAD_HALF => sign_extend(word.wrapping_shr(shift) & 0xffff, 16, self.arch_len),
            MEM_OP_LOAD_HALF_UNSIGNED => word.wrapping_shr(shift) & 0xffff,
            MEM_OP_LOAD_WORD => word,
            _ => 0,
        };
        result & self.arch_mask()
    }

    fn port(&self, space: MemSpace) -> &MemPortState {
        match space {
            MemSpace::Global => &self.global,
            MemSpace::Shared => &self.shared,
        }
    }

    fn port_mut(&mut self, space: MemSpace) -> &mut MemPortState {
        match space {
            MemSpace::Global => &mut self.global,
            MemSpace::Shared => &mut self.shared,
        }
    }

    fn queues_empty(&self, space: MemSpace, pending_request: bool) -> bool {
        self.port(space).queues_empty(pending_request)
    }

    fn source_busy(&self, space: MemSpace, tag: u64) -> bool {
        self.port(space).source_busy(tag)
    }

    fn commit_mem_request(
        &mut self,
        space: MemSpace,
        token: u64,
        op: u32,
        dest_reg: u32,
        tmask: u64,
        byte_offsets: Vec<u8>,
    ) {
        let tag = self.mem_tag(token);
        assert!(
            !self.source_busy(space, tag),
            "cyclotron LSU {} tag 0x{tag:x} reused before previous response retired",
            space.name()
        );
        let warp_id = self.token_warp_id(token);
        let debug_id = if is_load_op(op) {
            self.reservation_debug_ids.remove(&token).unwrap_or(0)
        } else {
            self.reservation_debug_ids.remove(&token);
            0
        };
        let port = self.port_mut(space);
        port.retired_tags.remove(&tag);

        if !is_load_op(op) {
            port.store_inflight.insert(tag, ());
            return;
        }

        let previous = port.inflight.insert(
            tag,
            MemInflight {
                token,
                op,
                warp_id,
                dest_reg,
                tmask,
                byte_offsets,
                debug_id,
            },
        );
        assert!(
            previous.is_none(),
            "cyclotron LSU {} tag 0x{tag:x} reused before previous response retired",
            space.name()
        );
    }

    fn commit_mem_response(&mut self, space: MemSpace, tag: u64) {
        let tag = self.base_mem_tag(tag);
        let port = self.port_mut(space);
        if port.inflight.remove(&tag).is_none() {
            if port.store_inflight.remove(&tag).is_some() {
                port.retired_tags.insert(tag, ());
                return;
            }
            if port.retired_tags.contains_key(&tag) {
                return;
            }
            assert!(
                port.inflight.is_empty() && port.store_inflight.is_empty(),
                "cyclotron LSU received unknown {} response tag 0x{tag:x}",
                space.name()
            );
        } else {
            port.retired_tags.insert(tag, ());
        }
    }
}

impl super::Context {
    fn lsu_index(cluster_id: u32, core_id: u32) -> usize {
        let cluster_id = cluster_id as usize;
        let core_id = core_id as usize;
        assert!(
            cluster_id < super::NUM_CLUSTERS,
            "cyclotron LSU cluster_id {} exceeds max {}",
            cluster_id,
            super::NUM_CLUSTERS
        );
        assert!(
            core_id < super::CORES_PER_CLUSTER,
            "cyclotron LSU core_id {} exceeds max {}",
            core_id,
            super::CORES_PER_CLUSTER
        );
        cluster_id * super::CORES_PER_CLUSTER + core_id
    }

    fn init_lsu(&mut self, cluster_id: u32, core_id: u32, model: CyclotronLsuModel) {
        let index = Self::lsu_index(cluster_id, core_id);
        self.lsu_models[index] = Some(model);
    }

    fn lsu_model_mut(&mut self, cluster_id: u32, core_id: u32) -> &mut CyclotronLsuModel {
        let index = Self::lsu_index(cluster_id, core_id);
        self.lsu_models[index]
            .as_mut()
            .expect("cyclotron LSU model not initialized")
    }
}

fn dpi_words(bits: usize) -> usize {
    (bits + 31) / 32
}

fn bit_mask(bits: usize) -> u32 {
    if bits == 0 {
        0
    } else if bits >= 32 {
        u32::MAX
    } else {
        (1u32 << bits) - 1
    }
}

fn bit_mask_u64(bits: usize) -> u64 {
    if bits == 0 {
        0
    } else if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

fn sign_extend(value: u32, from_bits: usize, to_bits: usize) -> u32 {
    assert!(
        from_bits > 0 && from_bits <= 32 && to_bits <= 32,
        "cyclotron LSU sign extension width is invalid"
    );
    let sign_bit = 1u32 << (from_bits - 1);
    let from_mask = bit_mask(from_bits);
    let to_mask = bit_mask(to_bits);
    let value = value & from_mask;
    if (value & sign_bit) != 0 {
        value | (!from_mask & to_mask)
    } else {
        value
    }
}

fn is_load_op(op: u32) -> bool {
    op <= 4
}

fn is_supported_memory_op(op: u32) -> bool {
    matches!(
        op,
        MEM_OP_LOAD_BYTE
            | MEM_OP_LOAD_BYTE_UNSIGNED
            | MEM_OP_LOAD_HALF
            | MEM_OP_LOAD_HALF_UNSIGNED
            | MEM_OP_LOAD_WORD
            | MEM_OP_STORE_BYTE
            | MEM_OP_STORE_HALF
            | MEM_OP_STORE_WORD
    )
}

fn is_store_like_op(op: u32) -> bool {
    (5..=15).contains(&op)
}

unsafe fn read_u32(value: *const u32) -> u32 {
    assert!(!value.is_null(), "cyclotron LSU scalar pointer is null");
    *value
}

unsafe fn read_bit(value: *const u32, bit: usize) -> bool {
    assert!(!value.is_null(), "cyclotron LSU bit vector pointer is null");
    ((*value.add(bit / 32) >> (bit % 32)) & 1) != 0
}

unsafe fn read_packed_field(value: *const u32, index: usize, width: usize) -> u32 {
    assert!(!value.is_null(), "cyclotron LSU bit vector pointer is null");
    assert!(
        width <= 32,
        "cyclotron LSU packed field width {} exceeds u32",
        width
    );
    let offset = index * width;
    let mut result = 0u32;
    for bit in 0..width {
        if read_bit(value, offset + bit) {
            result |= 1 << bit;
        }
    }
    result
}

unsafe fn read_packed_field_u64(value: *const u32, index: usize, width: usize) -> u64 {
    assert!(!value.is_null(), "cyclotron LSU bit vector pointer is null");
    assert!(
        width <= 64,
        "cyclotron LSU packed field width {} exceeds u64",
        width
    );
    let offset = index * width;
    let mut result = 0u64;
    for bit in 0..width {
        if read_bit(value, offset + bit) {
            result |= 1 << bit;
        }
    }
    result
}

unsafe fn read_bit_mask(value: *const u32, bits: usize) -> u64 {
    assert!(
        bits <= 64,
        "cyclotron LSU bit mask width {} exceeds u64",
        bits
    );
    let mut result = 0u64;
    for bit in 0..bits {
        if read_bit(value, bit) {
            result |= 1 << bit;
        }
    }
    result
}

unsafe fn zero_bit(value: *mut u8) {
    if let Some(value) = value.as_mut() {
        *value = 0;
    }
}

unsafe fn set_u8(value: *mut u8, new_value: u8) {
    if let Some(value) = value.as_mut() {
        *value = new_value;
    }
}

unsafe fn zero_words(value: *mut u32, words: usize) {
    if value.is_null() {
        return;
    }
    slice::from_raw_parts_mut(value, words).fill(0);
}

unsafe fn set_bit(value: *mut u32, bit: usize) {
    assert!(!value.is_null(), "cyclotron LSU bit vector pointer is null");
    *value.add(bit / 32) |= 1 << (bit % 32);
}

unsafe fn set_packed_field(value: *mut u32, index: usize, width: usize, field: u64) {
    assert!(!value.is_null(), "cyclotron LSU bit vector pointer is null");
    let offset = index * width;
    for bit in 0..width {
        if ((field >> bit) & 1) != 0 {
            set_bit(value, offset + bit);
        }
    }
}

unsafe fn set_bit_mask(value: *mut u32, bits: usize, mask: u64) {
    for bit in 0..bits {
        if ((mask >> bit) & 1) != 0 {
            set_bit(value, bit);
        }
    }
}

unsafe fn zero_mem_request(model: &CyclotronLsuModel, req: &MemRequestOutputs) {
    zero_bit(req.valid);
    zero_words(req.tag, model.mem_req_bits_tag_words);
    zero_words(req.op, model.mem_req_bits_op_words);
    zero_words(req.address, model.mem_req_bits_address_words);
    zero_words(req.data, model.mem_req_bits_data_words);
    zero_words(req.mask, model.mem_req_bits_mask_words);
    zero_words(req.tmask, model.mem_req_bits_tmask_words);
}

unsafe fn drive_mem_request(
    model: &CyclotronLsuModel,
    req: &MemRequestOutputs,
    token: u64,
    op: u32,
    core_req_bits_tmask: *const u32,
    core_req_bits_address: *const u32,
    core_req_bits_imm: *const u32,
    core_req_bits_store_data: *const u32,
) {
    let tag = model.mem_tag(token);
    set_u8(req.valid, 1);
    set_packed_field(req.tag, 0, model.source_id_bits, tag);
    set_packed_field(req.op, 0, model.mem_op_bits, op as u64);

    let imm = read_packed_field(core_req_bits_imm, 0, model.arch_len);
    for lane in 0..model.num_lsu_lanes {
        let base_address = read_packed_field(core_req_bits_address, lane, model.arch_len);
        let address = model.lane_address(base_address, imm);
        let store_data = read_packed_field(core_req_bits_store_data, lane, model.arch_len);
        let shifted_store_data = model.shifted_store_data(store_data, address);

        set_packed_field(req.address, lane, model.arch_len, address as u64);
        set_packed_field(req.data, lane, model.arch_len, shifted_store_data as u64);
        set_packed_field(
            req.mask,
            lane,
            model.per_lane_mask_bits,
            model.request_mask(op, address),
        );
        if read_bit(core_req_bits_tmask, lane) {
            set_bit(req.tmask, lane);
        }
    }
}

unsafe fn mem_request_fired(
    model: &CyclotronLsuModel,
    core_req_valid: u8,
    core_req_bits_token: *const u32,
    core_req_bits_op: *const u32,
    core_req_bits_tmask: *const u32,
    core_req_bits_address: *const u32,
    core_req_bits_imm: *const u32,
    core_req_bits_dest_reg: *const u32,
    global_mem_req_ready: u8,
    shmem_req_ready: u8,
) -> Option<(MemSpace, u64, u32, u32, u64, Vec<u8>)> {
    if core_req_valid == 0 {
        return None;
    }

    let token = read_packed_field_u64(core_req_bits_token, 0, model.token_bits);
    let op = read_packed_field(core_req_bits_op, 0, model.mem_op_bits);
    let space = model.request_space(token, op)?;
    let port_ready = match space {
        MemSpace::Global => global_mem_req_ready,
        MemSpace::Shared => shmem_req_ready,
    };
    if model.source_busy(space, model.mem_tag(token)) || port_ready == 0 {
        return None;
    }

    let dest_reg = read_packed_field(core_req_bits_dest_reg, 0, model.preg_bits);
    let tmask = read_bit_mask(core_req_bits_tmask, model.num_lsu_lanes);
    let imm = read_packed_field(core_req_bits_imm, 0, model.arch_len);
    let byte_offsets = (0..model.num_lsu_lanes)
        .map(|lane| {
            let base_address = read_packed_field(core_req_bits_address, lane, model.arch_len);
            model.byte_offset(model.lane_address(base_address, imm))
        })
        .collect();

    Some((space, token, op, dest_reg, tmask, byte_offsets))
}

unsafe fn drive_mem_response(
    model: &CyclotronLsuModel,
    resp: &MemResponseInputs,
    core_resp_ready: u8,
    core_resp_valid: *mut u8,
    core_resp_bits_warp_id: *mut u32,
    core_resp_bits_packet: *mut u32,
    core_resp_bits_tmask: *mut u32,
    core_resp_bits_dest_reg: *mut u32,
    core_resp_bits_writeback_data: *mut u32,
    core_resp_bits_debug_id: *mut u32,
) -> bool {
    if resp.valid == 0 {
        return false;
    }

    let tag = read_packed_field_u64(resp.tag, 0, model.source_id_bits);
    let base_tag = model.base_mem_tag(tag);
    let space = model
        .response_space(tag)
        .expect("cyclotron LSU response tag has invalid address space");
    let resp_valid_mask = read_bit_mask(resp.lane_valid, model.num_lsu_lanes);
    let port = model.port(space);

    if let Some(inflight) = port.inflight.get(&base_tag) {
        let needed_valid_mask = inflight.tmask & bit_mask_u64(model.num_lsu_lanes);
        let full_response = (resp_valid_mask & needed_valid_mask) == needed_valid_mask;

        if full_response {
            if inflight.is_load() {
                set_u8(core_resp_valid, 1);
                set_packed_field(
                    core_resp_bits_warp_id,
                    0,
                    model.warp_id_bits,
                    inflight.warp_id as u64,
                );
                set_packed_field(
                    core_resp_bits_packet,
                    0,
                    model.packet_bits,
                    model.packet_from_mem_tag(tag),
                );
                set_bit_mask(
                    core_resp_bits_tmask,
                    model.num_lsu_lanes,
                    needed_valid_mask & resp_valid_mask,
                );
                set_packed_field(
                    core_resp_bits_dest_reg,
                    0,
                    model.preg_bits,
                    inflight.dest_reg as u64,
                );
                for lane in 0..model.num_lsu_lanes {
                    let word = read_packed_field(resp.data, lane, model.arch_len);
                    let byte_offset = inflight.byte_offsets.get(lane).copied().unwrap_or(0);
                    let data = model.load_writeback_data(inflight.op, word, byte_offset);
                    set_packed_field(
                        core_resp_bits_writeback_data,
                        lane,
                        model.arch_len,
                        data as u64,
                    );
                }
                set_packed_field(
                    core_resp_bits_debug_id,
                    0,
                    model.debug_id_port_bits,
                    inflight.debug_id as u64,
                );
                set_u8(resp.ready, u8::from(core_resp_ready != 0));
            } else {
                set_u8(resp.ready, 1);
            }
        }
    } else if port.retired_tags.contains_key(&base_tag)
        || port.store_inflight.contains_key(&base_tag)
        || (port.inflight.is_empty() && port.store_inflight.is_empty())
    {
        set_u8(resp.ready, 1);
    } else {
        // This is combinational DPI and may observe transient valid/tag
        // combinations while upstream RTL settles. Leave ready low; commit
        // validates only stable responses that actually fired.
    }

    true
}

unsafe fn mem_response_fired(
    model: &CyclotronLsuModel,
    resp_valid: u8,
    resp_ready: u8,
    resp_bits_tag: *const u32,
    resp_bits_valid: *const u32,
    core_resp_ready: u8,
) -> (bool, Option<(MemSpace, u64)>) {
    if resp_valid == 0 {
        return (false, None);
    }
    if resp_ready == 0 {
        return (true, None);
    }

    let tag = read_packed_field_u64(resp_bits_tag, 0, model.source_id_bits);
    let base_tag = model.base_mem_tag(tag);
    let space = model
        .response_space(tag)
        .expect("cyclotron LSU response tag has invalid address space");
    let port = model.port(space);
    if let Some(inflight) = port.inflight.get(&base_tag) {
        let resp_valid_mask = read_bit_mask(resp_bits_valid, model.num_lsu_lanes);
        let needed_valid_mask = inflight.tmask & bit_mask_u64(model.num_lsu_lanes);
        let full_response = (resp_valid_mask & needed_valid_mask) == needed_valid_mask;
        if full_response && (!inflight.is_load() || core_resp_ready != 0) {
            (true, Some((space, tag)))
        } else {
            (true, None)
        }
    } else if port.retired_tags.contains_key(&base_tag) {
        (true, None)
    } else if port.store_inflight.contains_key(&base_tag)
        || (port.inflight.is_empty() && port.store_inflight.is_empty())
    {
        (true, Some((space, tag)))
    } else {
        panic!(
            "cyclotron LSU received unknown {} response tag 0x{tag:x}",
            space.name()
        );
    }
}

#[no_mangle]
pub unsafe extern "C" fn cyclotron_lsu_init_rs(
    cluster_id: *const u32,
    core_id: *const u32,
    arch_len: c_int,
    num_warps: c_int,
    num_lanes: c_int,
    num_lsu_lanes: c_int,
    _cluster_id_bits: c_int,
    _core_id_bits: c_int,
    warp_id_bits: c_int,
    token_bits: c_int,
    _address_space_bits: c_int,
    mem_op_bits: c_int,
    preg_bits: c_int,
    packet_bits: c_int,
    source_id_bits: c_int,
    per_lane_mask_bits: c_int,
    _debug_id_bits: c_int,
    debug_id_port_bits: c_int,
) {
    let cluster_id = read_u32(cluster_id);
    let core_id = read_u32(core_id);
    let model = CyclotronLsuModel::new(
        arch_len,
        num_warps,
        num_lanes,
        num_lsu_lanes,
        warp_id_bits,
        token_bits,
        _address_space_bits,
        mem_op_bits,
        preg_bits,
        packet_bits,
        source_id_bits,
        per_lane_mask_bits,
        debug_id_port_bits,
    );

    let mut context_guard = super::CELL.write().unwrap();
    let context = context_guard
        .as_mut()
        .expect("DPI context not initialized!");
    context.init_lsu(cluster_id, core_id, model);
}

#[no_mangle]
pub unsafe extern "C" fn cyclotron_lsu_reset_rs(cluster_id: *const u32, core_id: *const u32) {
    let cluster_id = read_u32(cluster_id);
    let core_id = read_u32(core_id);
    let mut context_guard = super::CELL.write().unwrap();
    let context = context_guard
        .as_mut()
        .expect("DPI context not initialized!");
    context.lsu_model_mut(cluster_id, core_id).reset();
}

#[no_mangle]
/// Combinational logic.  Drives RTL with zero-cycle latency.
pub unsafe extern "C" fn cyclotron_lsu_eval_rs(
    cluster_id: *const u32,
    core_id: *const u32,
    core_reservations_req_valid: *const u32,
    core_reservations_req_bits_address_space: *const u32,
    core_reservations_req_bits_op: *const u32,
    _core_reservations_req_bits_debug_id: *const u32,
    core_req_valid: u8,
    core_req_bits_token: *const u32,
    core_req_bits_op: *const u32,
    core_req_bits_tmask: *const u32,
    core_req_bits_address: *const u32,
    core_req_bits_imm: *const u32,
    _core_req_bits_dest_reg: *const u32,
    core_req_bits_store_data: *const u32,
    core_resp_ready: u8,
    global_mem_req_ready: u8,
    global_mem_resp_valid: u8,
    global_mem_resp_bits_tag: *const u32,
    global_mem_resp_bits_valid: *const u32,
    global_mem_resp_bits_data: *const u32,
    shmem_req_ready: u8,
    shmem_resp_valid: u8,
    shmem_resp_bits_tag: *const u32,
    shmem_resp_bits_valid: *const u32,
    shmem_resp_bits_data: *const u32,
    core_reservations_req_ready: *mut u32,
    core_reservations_resp_valid: *mut u32,
    core_reservations_resp_bits_token: *mut u32,
    core_req_ready: *mut u8,
    core_resp_valid: *mut u8,
    core_resp_bits_warp_id: *mut u32,
    core_resp_bits_packet: *mut u32,
    core_resp_bits_tmask: *mut u32,
    core_resp_bits_dest_reg: *mut u32,
    core_resp_bits_writeback_data: *mut u32,
    core_resp_bits_debug_id: *mut u32,
    global_mem_req_valid: *mut u8,
    global_mem_req_bits_tag: *mut u32,
    global_mem_req_bits_op: *mut u32,
    global_mem_req_bits_address: *mut u32,
    global_mem_req_bits_data: *mut u32,
    global_mem_req_bits_mask: *mut u32,
    global_mem_req_bits_tmask: *mut u32,
    global_mem_resp_ready: *mut u8,
    shmem_req_valid: *mut u8,
    shmem_req_bits_tag: *mut u32,
    shmem_req_bits_op: *mut u32,
    shmem_req_bits_address: *mut u32,
    shmem_req_bits_data: *mut u32,
    shmem_req_bits_mask: *mut u32,
    shmem_req_bits_tmask: *mut u32,
    shmem_resp_ready: *mut u8,
    shared_queues_empty: *mut u8,
    global_queues_empty: *mut u8,
) {
    let cluster_id = read_u32(cluster_id);
    let core_id = read_u32(core_id);
    let mut context_guard = super::CELL.write().unwrap();
    let context = context_guard
        .as_mut()
        .expect("DPI context not initialized!");
    let model = context.lsu_model_mut(cluster_id, core_id);

    zero_words(
        core_reservations_req_ready,
        model.core_reservations_req_ready_words,
    );
    zero_words(
        core_reservations_resp_valid,
        model.core_reservations_resp_valid_words,
    );
    zero_words(
        core_reservations_resp_bits_token,
        model.core_reservations_resp_bits_token_words,
    );

    for warp_id in 0..model.num_warps {
        if !read_bit(core_reservations_req_valid, warp_id) {
            continue;
        }

        let address_space = read_packed_field(
            core_reservations_req_bits_address_space,
            warp_id,
            model.address_space_bits,
        );
        let op = read_packed_field(core_reservations_req_bits_op, warp_id, model.mem_op_bits);
        if let Some(token) = model.reservation_token(warp_id, address_space, op) {
            set_bit(core_reservations_req_ready, warp_id);
            set_bit(core_reservations_resp_valid, warp_id);
            set_packed_field(
                core_reservations_resp_bits_token,
                warp_id,
                model.token_bits,
                token,
            );
        }
    }

    zero_bit(core_req_ready);
    zero_bit(core_resp_valid);
    zero_words(core_resp_bits_warp_id, model.core_resp_bits_warp_id_words);
    zero_words(core_resp_bits_packet, model.core_resp_bits_packet_words);
    zero_words(core_resp_bits_tmask, model.core_resp_bits_tmask_words);
    zero_words(core_resp_bits_dest_reg, model.core_resp_bits_dest_reg_words);
    zero_words(
        core_resp_bits_writeback_data,
        model.core_resp_bits_writeback_data_words,
    );
    zero_words(core_resp_bits_debug_id, model.core_resp_bits_debug_id_words);
    let global_mem_req = MemRequestOutputs {
        valid: global_mem_req_valid,
        tag: global_mem_req_bits_tag,
        op: global_mem_req_bits_op,
        address: global_mem_req_bits_address,
        data: global_mem_req_bits_data,
        mask: global_mem_req_bits_mask,
        tmask: global_mem_req_bits_tmask,
    };
    let shmem_req = MemRequestOutputs {
        valid: shmem_req_valid,
        tag: shmem_req_bits_tag,
        op: shmem_req_bits_op,
        address: shmem_req_bits_address,
        data: shmem_req_bits_data,
        mask: shmem_req_bits_mask,
        tmask: shmem_req_bits_tmask,
    };

    zero_mem_request(model, &global_mem_req);
    zero_bit(global_mem_resp_ready);
    zero_mem_request(model, &shmem_req);
    zero_bit(shmem_resp_ready);

    let mut pending_global_request = false;
    let mut pending_shared_request = false;
    if core_req_valid != 0 {
        let token = read_packed_field_u64(core_req_bits_token, 0, model.token_bits);
        let op = read_packed_field(core_req_bits_op, 0, model.mem_op_bits);
        if let Some(space) = model.request_space(token, op) {
            match space {
                MemSpace::Global => pending_global_request = true,
                MemSpace::Shared => pending_shared_request = true,
            }
            let tag = model.mem_tag(token);
            if !model.source_busy(space, tag) {
                let (req_ready, req_outputs) = match space {
                    MemSpace::Global => (global_mem_req_ready, &global_mem_req),
                    MemSpace::Shared => (shmem_req_ready, &shmem_req),
                };
                set_u8(core_req_ready, u8::from(req_ready != 0));
                drive_mem_request(
                    model,
                    req_outputs,
                    token,
                    op,
                    core_req_bits_tmask,
                    core_req_bits_address,
                    core_req_bits_imm,
                    core_req_bits_store_data,
                );
            }
        }
    }

    let global_mem_resp = MemResponseInputs {
        valid: global_mem_resp_valid,
        tag: global_mem_resp_bits_tag,
        lane_valid: global_mem_resp_bits_valid,
        data: global_mem_resp_bits_data,
        ready: global_mem_resp_ready,
    };
    let shmem_resp = MemResponseInputs {
        valid: shmem_resp_valid,
        tag: shmem_resp_bits_tag,
        lane_valid: shmem_resp_bits_valid,
        data: shmem_resp_bits_data,
        ready: shmem_resp_ready,
    };

    let global_response_selected = drive_mem_response(
        model,
        &global_mem_resp,
        core_resp_ready,
        core_resp_valid,
        core_resp_bits_warp_id,
        core_resp_bits_packet,
        core_resp_bits_tmask,
        core_resp_bits_dest_reg,
        core_resp_bits_writeback_data,
        core_resp_bits_debug_id,
    );
    if !global_response_selected {
        drive_mem_response(
            model,
            &shmem_resp,
            core_resp_ready,
            core_resp_valid,
            core_resp_bits_warp_id,
            core_resp_bits_packet,
            core_resp_bits_tmask,
            core_resp_bits_dest_reg,
            core_resp_bits_writeback_data,
            core_resp_bits_debug_id,
        );
    }

    if let Some(value) = shared_queues_empty.as_mut() {
        *value = u8::from(model.queues_empty(MemSpace::Shared, pending_shared_request));
    }
    if let Some(value) = global_queues_empty.as_mut() {
        *value = u8::from(model.queues_empty(MemSpace::Global, pending_global_request));
    }
}

#[no_mangle]
/// Sequential logic.  Updates Rust state such as inflight table; does not drive
/// RTL.
pub unsafe extern "C" fn cyclotron_lsu_commit_rs(
    cluster_id: *const u32,
    core_id: *const u32,
    core_reservations_req_valid: *const u32,
    core_reservations_req_bits_address_space: *const u32,
    core_reservations_req_bits_op: *const u32,
    core_reservations_req_bits_debug_id: *const u32,
    core_req_valid: u8,
    core_req_bits_token: *const u32,
    core_req_bits_op: *const u32,
    core_req_bits_tmask: *const u32,
    core_req_bits_address: *const u32,
    core_req_bits_imm: *const u32,
    core_req_bits_dest_reg: *const u32,
    _core_req_bits_store_data: *const u32,
    core_resp_ready: u8,
    global_mem_req_ready: u8,
    global_mem_resp_valid: u8,
    global_mem_resp_bits_tag: *const u32,
    global_mem_resp_bits_valid: *const u32,
    _global_mem_resp_bits_data: *const u32,
    shmem_req_ready: u8,
    shmem_resp_valid: u8,
    shmem_resp_bits_tag: *const u32,
    shmem_resp_bits_valid: *const u32,
    _shmem_resp_bits_data: *const u32,
    _core_reservations_req_ready: *const u32,
    _core_reservations_resp_valid: *const u32,
    _core_reservations_resp_bits_token: *const u32,
    _core_req_ready: u8,
    _core_resp_valid: u8,
    _core_resp_bits_warp_id: *const u32,
    _core_resp_bits_packet: *const u32,
    _core_resp_bits_tmask: *const u32,
    _core_resp_bits_dest_reg: *const u32,
    _core_resp_bits_writeback_data: *const u32,
    _core_resp_bits_debug_id: *const u32,
    _global_mem_req_valid: u8,
    _global_mem_req_bits_tag: *const u32,
    _global_mem_req_bits_op: *const u32,
    _global_mem_req_bits_address: *const u32,
    _global_mem_req_bits_data: *const u32,
    _global_mem_req_bits_mask: *const u32,
    _global_mem_req_bits_tmask: *const u32,
    global_mem_resp_ready: u8,
    _shmem_req_valid: u8,
    _shmem_req_bits_tag: *const u32,
    _shmem_req_bits_op: *const u32,
    _shmem_req_bits_address: *const u32,
    _shmem_req_bits_data: *const u32,
    _shmem_req_bits_mask: *const u32,
    _shmem_req_bits_tmask: *const u32,
    shmem_resp_ready: u8,
    _shared_queues_empty: u8,
    _global_queues_empty: u8,
) {
    let cluster_id = read_u32(cluster_id);
    let core_id = read_u32(core_id);
    let mut context_guard = super::CELL.write().unwrap();
    let context = context_guard
        .as_mut()
        .expect("DPI context not initialized!");
    let model = context.lsu_model_mut(cluster_id, core_id);

    let request_fired = mem_request_fired(
        model,
        core_req_valid,
        core_req_bits_token,
        core_req_bits_op,
        core_req_bits_tmask,
        core_req_bits_address,
        core_req_bits_imm,
        core_req_bits_dest_reg,
        global_mem_req_ready,
        shmem_req_ready,
    );

    let (global_response_selected, global_response_fired) = mem_response_fired(
        model,
        global_mem_resp_valid,
        global_mem_resp_ready,
        global_mem_resp_bits_tag,
        global_mem_resp_bits_valid,
        core_resp_ready,
    );
    let shared_response_fired = if global_response_selected {
        None
    } else {
        let (_, fired) = mem_response_fired(
            model,
            shmem_resp_valid,
            shmem_resp_ready,
            shmem_resp_bits_tag,
            shmem_resp_bits_valid,
            core_resp_ready,
        );
        fired
    };

    for warp_id in 0..model.num_warps {
        if !read_bit(core_reservations_req_valid, warp_id) {
            continue;
        }

        let address_space = read_packed_field(
            core_reservations_req_bits_address_space,
            warp_id,
            model.address_space_bits,
        );
        let op = read_packed_field(core_reservations_req_bits_op, warp_id, model.mem_op_bits);
        let debug_id = read_packed_field(
            core_reservations_req_bits_debug_id,
            warp_id,
            model.debug_id_port_bits,
        );
        if model
            .reservation_token(warp_id, address_space, op)
            .is_some()
        {
            model.commit_reservation(warp_id, address_space, op, debug_id);
        }
    }

    if let Some((space, tag)) = global_response_fired {
        model.commit_mem_response(space, tag);
    }
    if let Some((space, tag)) = shared_response_fired {
        model.commit_mem_response(space, tag);
    }
    if let Some((space, token, op, dest_reg, tmask, byte_offsets)) = request_fired {
        model.commit_mem_request(space, token, op, dest_reg, tmask, byte_offsets);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_model() -> CyclotronLsuModel {
        CyclotronLsuModel::new(
            32, // arch_len
            8,  // num_warps
            16, // num_lanes
            16, // num_lsu_lanes
            3,  // warp_id_bits
            9,  // token_bits: warpId(3), addressSpace(1), ldq(1), index(4)
            1,  // address_space_bits
            4,  // mem_op_bits
            8,  // preg_bits
            1,  // packet_bits: Chisel uses a 1-bit zero packet field
            10, // source_id_bits
            4,  // per_lane_mask_bits
            1,  // debug_id_port_bits
        )
    }

    #[test]
    fn packs_reservation_token_in_chisel_layout() {
        let model = make_model();

        let global_load_word = model.reservation_token(5, 0, MEM_OP_LOAD_WORD).unwrap();
        assert_eq!(global_load_word, (5 << 6) | (1 << 4));

        let shared_store_word = model.reservation_token(2, 1, MEM_OP_STORE_WORD).unwrap();
        assert_eq!(shared_store_word, (2 << 6) | (1 << 5));
    }

    #[test]
    fn advances_only_matching_reservation_counter() {
        let mut model = make_model();

        model.commit_reservation(0, 0, MEM_OP_LOAD_WORD, 0);
        assert_eq!(
            model.reservation_token(0, 0, MEM_OP_LOAD_WORD).unwrap(),
            (1 << 4) | 1
        );
        assert_eq!(model.reservation_token(0, 0, MEM_OP_STORE_WORD).unwrap(), 0);
        assert_eq!(
            model.reservation_token(0, 1, MEM_OP_LOAD_WORD).unwrap(),
            (1 << 5) | (1 << 4)
        );
    }

    fn zero_byte_offsets() -> Vec<u8> {
        vec![0; 16]
    }

    #[test]
    fn accepts_global_and_shared_core_requests() {
        let model = make_model();

        let global_load_word = model.reservation_token(1, 0, MEM_OP_LOAD_WORD).unwrap();
        let global_store_word = model.reservation_token(1, 0, MEM_OP_STORE_WORD).unwrap();
        let shared_load_word = model.reservation_token(1, 1, MEM_OP_LOAD_WORD).unwrap();
        let shared_store_word = model.reservation_token(1, 1, MEM_OP_STORE_WORD).unwrap();

        for op in MEM_OP_LOAD_BYTE..=MEM_OP_STORE_WORD {
            assert_eq!(
                model.request_space(global_load_word, op),
                Some(MemSpace::Global)
            );
            assert_eq!(
                model.request_space(shared_load_word, op),
                Some(MemSpace::Shared)
            );
        }
        assert_eq!(
            model.request_space(global_store_word, MEM_OP_STORE_WORD),
            Some(MemSpace::Global)
        );
        assert_eq!(
            model.request_space(shared_store_word, MEM_OP_STORE_WORD),
            Some(MemSpace::Shared)
        );
        assert_eq!(model.request_space(global_load_word, 8), None);
    }

    #[test]
    fn builds_subword_memory_request_masks_and_store_data() {
        let model = make_model();

        assert_eq!(model.request_mask(MEM_OP_LOAD_BYTE, 0x1001), 0b0010);
        assert_eq!(model.request_mask(MEM_OP_STORE_BYTE, 0x1003), 0b1000);
        assert_eq!(model.request_mask(MEM_OP_LOAD_HALF, 0x1000), 0b0011);
        assert_eq!(model.request_mask(MEM_OP_STORE_HALF, 0x1002), 0b1100);
        assert_eq!(model.request_mask(MEM_OP_LOAD_WORD, 0x1000), 0b1111);

        assert_eq!(model.shifted_store_data(0xa5, 0x1001), 0x0000_a500);
        assert_eq!(model.shifted_store_data(0xbeef, 0x1002), 0xbeef_0000);
    }

    #[test]
    fn extracts_and_extends_subword_load_data() {
        let model = make_model();
        let word = 0x807f_3412;

        assert_eq!(model.load_writeback_data(MEM_OP_LOAD_BYTE, word, 0), 0x12);
        assert_eq!(
            model.load_writeback_data(MEM_OP_LOAD_BYTE, word, 3),
            0xffff_ff80
        );
        assert_eq!(
            model.load_writeback_data(MEM_OP_LOAD_BYTE_UNSIGNED, word, 3),
            0x80
        );
        assert_eq!(model.load_writeback_data(MEM_OP_LOAD_HALF, word, 0), 0x3412);
        assert_eq!(
            model.load_writeback_data(MEM_OP_LOAD_HALF, word, 2),
            0xffff_807f
        );
        assert_eq!(
            model.load_writeback_data(MEM_OP_LOAD_HALF_UNSIGNED, word, 2),
            0x807f
        );
        assert_eq!(model.load_writeback_data(MEM_OP_LOAD_WORD, word, 0), word);
    }

    #[test]
    fn packs_memory_tag_as_token_then_packet_zero() {
        let model = make_model();
        let token = model.reservation_token(2, 0, MEM_OP_LOAD_WORD).unwrap();

        assert_eq!(model.mem_tag(token), token << 1);
        assert_eq!(model.packet_from_mem_tag(model.mem_tag(token)), 0);
    }

    #[test]
    fn retires_response_with_nonzero_packet_bit_against_base_tag() {
        let mut model = make_model();
        let token = model.reservation_token(1, 0, MEM_OP_STORE_WORD).unwrap();
        let base_tag = model.mem_tag(token);
        let packet_one_tag = base_tag | 1;

        model.commit_mem_request(
            MemSpace::Global,
            token,
            MEM_OP_STORE_WORD,
            0,
            1,
            zero_byte_offsets(),
        );

        model.commit_mem_response(MemSpace::Global, packet_one_tag);

        assert!(!model.source_busy(MemSpace::Global, base_tag));
        assert!(model.global.retired_tags.contains_key(&base_tag));
    }

    #[test]
    fn tracks_global_queue_empty_with_inflight_request() {
        let mut model = make_model();

        assert!(model.queues_empty(MemSpace::Global, false));
        assert!(!model.queues_empty(MemSpace::Global, true));

        let token = model.reservation_token(0, 0, MEM_OP_LOAD_WORD).unwrap();
        model.commit_mem_request(
            MemSpace::Global,
            token,
            MEM_OP_LOAD_WORD,
            3,
            0xffff,
            zero_byte_offsets(),
        );
        assert!(!model.queues_empty(MemSpace::Global, false));

        model.commit_mem_response(MemSpace::Global, model.mem_tag(token));
        assert!(model.queues_empty(MemSpace::Global, false));
    }

    #[test]
    fn tracks_store_inflight_by_source_tag() {
        let mut model = make_model();
        let token = model.reservation_token(0, 0, MEM_OP_STORE_WORD).unwrap();
        let tag = model.mem_tag(token);

        model.commit_mem_request(
            MemSpace::Global,
            token,
            MEM_OP_STORE_WORD,
            0,
            1,
            zero_byte_offsets(),
        );
        assert!(model.source_busy(MemSpace::Global, tag));
        assert!(!model.queues_empty(MemSpace::Global, false));

        model.commit_mem_response(MemSpace::Global, tag);
        assert!(!model.source_busy(MemSpace::Global, tag));
        assert!(model.queues_empty(MemSpace::Global, false));
    }

    #[test]
    fn tracks_shared_queue_empty_with_inflight_request() {
        let mut model = make_model();

        assert!(model.queues_empty(MemSpace::Shared, false));
        assert!(!model.queues_empty(MemSpace::Shared, true));

        let token = model.reservation_token(0, 1, MEM_OP_LOAD_WORD).unwrap();
        model.commit_mem_request(
            MemSpace::Shared,
            token,
            MEM_OP_LOAD_WORD,
            3,
            0xffff,
            zero_byte_offsets(),
        );
        assert!(!model.queues_empty(MemSpace::Shared, false));
        assert!(model.queues_empty(MemSpace::Global, false));

        model.commit_mem_response(MemSpace::Shared, model.mem_tag(token));
        assert!(model.queues_empty(MemSpace::Shared, false));
    }

    #[test]
    fn moves_reservation_debug_id_into_global_inflight_metadata() {
        let mut model = make_model();

        let token = model.reservation_token(3, 0, MEM_OP_LOAD_WORD).unwrap();
        model.commit_reservation(3, 0, MEM_OP_LOAD_WORD, 1);
        model.commit_mem_request(
            MemSpace::Global,
            token,
            MEM_OP_LOAD_WORD,
            7,
            0xaaaa,
            zero_byte_offsets(),
        );

        let metadata = model.global.inflight.get(&model.mem_tag(token)).unwrap();
        assert_eq!(
            metadata,
            &MemInflight {
                token,
                op: MEM_OP_LOAD_WORD,
                warp_id: 3,
                dest_reg: 7,
                tmask: 0xaaaa,
                byte_offsets: zero_byte_offsets(),
                debug_id: 1,
            }
        );
    }

    #[test]
    fn tolerates_duplicate_retired_response_while_new_request_inflight() {
        let mut model = make_model();

        let retired_token = model.reservation_token(0, 0, MEM_OP_LOAD_WORD).unwrap();
        let retired_tag = model.mem_tag(retired_token);
        model.commit_mem_request(
            MemSpace::Global,
            retired_token,
            MEM_OP_LOAD_WORD,
            3,
            0xffff,
            zero_byte_offsets(),
        );
        model.commit_mem_response(MemSpace::Global, retired_tag);

        let active_token = model.reservation_token(1, 0, MEM_OP_LOAD_WORD).unwrap();
        let active_tag = model.mem_tag(active_token);
        model.commit_mem_request(
            MemSpace::Global,
            active_token,
            MEM_OP_LOAD_WORD,
            4,
            0xffff,
            zero_byte_offsets(),
        );

        model.commit_mem_response(MemSpace::Global, retired_tag);
        assert!(model.global.inflight.contains_key(&active_tag));
    }
}
