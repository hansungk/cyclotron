use std::collections::HashMap;
use std::os::raw::c_int;
use std::slice;

const ADDRESS_SPACE_GLOBAL_MEMORY: u32 = 0;
const MEM_OP_LOAD_BYTE: u32 = 0;
const MEM_OP_LOAD_BYTE_UNSIGNED: u32 = 1;
const MEM_OP_LOAD_HALF: u32 = 2;
const MEM_OP_LOAD_HALF_UNSIGNED: u32 = 3;
const MEM_OP_LOAD_WORD: u32 = 4;
const MEM_OP_STORE_BYTE: u32 = 5;
const MEM_OP_STORE_HALF: u32 = 6;
const MEM_OP_STORE_WORD: u32 = 7;

#[derive(Clone, Debug, PartialEq, Eq)]
struct GlobalInflight {
    token: u64,
    op: u32,
    warp_id: u32,
    dest_reg: u32,
    tmask: u64,
    byte_offsets: Vec<u8>,
    debug_id: u32,
}

impl GlobalInflight {
    fn is_load(&self) -> bool {
        is_load_op(self.op)
    }
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
    global_inflight: HashMap<u64, GlobalInflight>,
    global_store_inflight: HashMap<u64, ()>,
    global_retired_tags: HashMap<u64, ()>,
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
            global_inflight: HashMap::new(),
            global_store_inflight: HashMap::new(),
            global_retired_tags: HashMap::new(),
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
        self.global_inflight.clear();
        self.global_store_inflight.clear();
        self.global_retired_tags.clear();
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

    fn supports_global_request(&self, token: u64, op: u32) -> bool {
        self.token_address_space(token) == ADDRESS_SPACE_GLOBAL_MEMORY
            && matches!(
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

    fn mem_tag(&self, token: u64) -> u64 {
        token << self.packet_bits
    }

    fn packet_from_mem_tag(&self, tag: u64) -> u64 {
        tag & bit_mask_u64(self.packet_bits)
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

    fn global_queues_empty(&self, pending_request: bool) -> bool {
        self.global_inflight.is_empty() && self.global_store_inflight.is_empty() && !pending_request
    }

    fn global_source_busy(&self, tag: u64) -> bool {
        self.global_inflight.contains_key(&tag) || self.global_store_inflight.contains_key(&tag)
    }

    fn commit_global_request(
        &mut self,
        token: u64,
        op: u32,
        dest_reg: u32,
        tmask: u64,
        byte_offsets: Vec<u8>,
    ) {
        let tag = self.mem_tag(token);
        assert!(
            !self.global_source_busy(tag),
            "cyclotron LSU global tag 0x{tag:x} reused before previous response retired"
        );
        self.global_retired_tags.remove(&tag);

        if !is_load_op(op) {
            self.reservation_debug_ids.remove(&token);
            self.global_store_inflight.insert(tag, ());
            return;
        }

        let debug_id = self.reservation_debug_ids.remove(&token).unwrap_or(0);
        let previous = self.global_inflight.insert(
            tag,
            GlobalInflight {
                token,
                op,
                warp_id: self.token_warp_id(token),
                dest_reg,
                tmask,
                byte_offsets,
                debug_id,
            },
        );
        assert!(
            previous.is_none(),
            "cyclotron LSU global tag 0x{tag:x} reused before previous response retired"
        );
    }

    fn commit_global_response(&mut self, tag: u64) {
        if self.global_inflight.remove(&tag).is_none() {
            if self.global_store_inflight.remove(&tag).is_some() {
                self.global_retired_tags.insert(tag, ());
                return;
            }
            if self.global_retired_tags.contains_key(&tag) {
                return;
            }
            assert!(
                self.global_inflight.is_empty() && self.global_store_inflight.is_empty(),
                "cyclotron LSU received unknown global response tag 0x{tag:x}"
            );
        } else {
            self.global_retired_tags.insert(tag, ());
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
    _shmem_req_ready: u8,
    _shmem_resp_valid: u8,
    _shmem_resp_bits_tag: *const u32,
    _shmem_resp_bits_valid: *const u32,
    _shmem_resp_bits_data: *const u32,
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
    zero_bit(global_mem_req_valid);
    zero_words(global_mem_req_bits_tag, model.mem_req_bits_tag_words);
    zero_words(global_mem_req_bits_op, model.mem_req_bits_op_words);
    zero_words(
        global_mem_req_bits_address,
        model.mem_req_bits_address_words,
    );
    zero_words(global_mem_req_bits_data, model.mem_req_bits_data_words);
    zero_words(global_mem_req_bits_mask, model.mem_req_bits_mask_words);
    zero_words(global_mem_req_bits_tmask, model.mem_req_bits_tmask_words);
    zero_bit(global_mem_resp_ready);
    zero_bit(shmem_req_valid);
    zero_words(shmem_req_bits_tag, model.mem_req_bits_tag_words);
    zero_words(shmem_req_bits_op, model.mem_req_bits_op_words);
    zero_words(shmem_req_bits_address, model.mem_req_bits_address_words);
    zero_words(shmem_req_bits_data, model.mem_req_bits_data_words);
    zero_words(shmem_req_bits_mask, model.mem_req_bits_mask_words);
    zero_words(shmem_req_bits_tmask, model.mem_req_bits_tmask_words);
    zero_bit(shmem_resp_ready);

    let mut pending_global_request = false;
    if core_req_valid != 0 {
        let token = read_packed_field_u64(core_req_bits_token, 0, model.token_bits);
        let op = read_packed_field(core_req_bits_op, 0, model.mem_op_bits);
        if model.supports_global_request(token, op) {
            pending_global_request = true;
            let tag = model.mem_tag(token);
            if !model.global_source_busy(tag) {
                set_u8(core_req_ready, u8::from(global_mem_req_ready != 0));
                set_u8(global_mem_req_valid, 1);
                set_packed_field(global_mem_req_bits_tag, 0, model.source_id_bits, tag);
                set_packed_field(global_mem_req_bits_op, 0, model.mem_op_bits, op as u64);

                let imm = read_packed_field(core_req_bits_imm, 0, model.arch_len);
                for lane in 0..model.num_lsu_lanes {
                    let base_address =
                        read_packed_field(core_req_bits_address, lane, model.arch_len);
                    let address = model.lane_address(base_address, imm);
                    let store_data =
                        read_packed_field(core_req_bits_store_data, lane, model.arch_len);
                    let shifted_store_data = model.shifted_store_data(store_data, address);

                    set_packed_field(
                        global_mem_req_bits_address,
                        lane,
                        model.arch_len,
                        address as u64,
                    );
                    set_packed_field(
                        global_mem_req_bits_data,
                        lane,
                        model.arch_len,
                        shifted_store_data as u64,
                    );
                    set_packed_field(
                        global_mem_req_bits_mask,
                        lane,
                        model.per_lane_mask_bits,
                        model.request_mask(op, address),
                    );
                    if read_bit(core_req_bits_tmask, lane) {
                        set_bit(global_mem_req_bits_tmask, lane);
                    }
                }
            }
        }
    }

    if global_mem_resp_valid != 0 {
        let tag = read_packed_field_u64(global_mem_resp_bits_tag, 0, model.source_id_bits);
        let resp_valid_mask = read_bit_mask(global_mem_resp_bits_valid, model.num_lsu_lanes);
        if let Some(inflight) = model.global_inflight.get(&tag) {
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
                        let word =
                            read_packed_field(global_mem_resp_bits_data, lane, model.arch_len);
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
                    set_u8(global_mem_resp_ready, u8::from(core_resp_ready != 0));
                } else {
                    set_u8(global_mem_resp_ready, 1);
                }
            }
        } else if model.global_retired_tags.contains_key(&tag)
            || model.global_store_inflight.contains_key(&tag)
            || (model.global_inflight.is_empty() && model.global_store_inflight.is_empty())
        {
            set_u8(global_mem_resp_ready, 1);
        } else {
            panic!("cyclotron LSU received unknown global response tag 0x{tag:x}");
        }
    }

    if let Some(value) = shared_queues_empty.as_mut() {
        *value = 1;
    }
    if let Some(value) = global_queues_empty.as_mut() {
        *value = u8::from(model.global_queues_empty(pending_global_request));
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
    _shmem_req_ready: u8,
    _shmem_resp_valid: u8,
    _shmem_resp_bits_tag: *const u32,
    _shmem_resp_bits_valid: *const u32,
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
    _global_mem_resp_ready: u8,
    _shmem_req_valid: u8,
    _shmem_req_bits_tag: *const u32,
    _shmem_req_bits_op: *const u32,
    _shmem_req_bits_address: *const u32,
    _shmem_req_bits_data: *const u32,
    _shmem_req_bits_mask: *const u32,
    _shmem_req_bits_tmask: *const u32,
    _shmem_resp_ready: u8,
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
    let visible_global_response_tag = if global_mem_resp_valid != 0 {
        Some(read_packed_field_u64(
            global_mem_resp_bits_tag,
            0,
            model.source_id_bits,
        ))
    } else {
        None
    };

    let global_request_fired = if core_req_valid != 0 {
        let token = read_packed_field_u64(core_req_bits_token, 0, model.token_bits);
        let op = read_packed_field(core_req_bits_op, 0, model.mem_op_bits);
        if model.supports_global_request(token, op)
            && !model.global_source_busy(model.mem_tag(token))
            && global_mem_req_ready != 0
        {
            let dest_reg = read_packed_field(core_req_bits_dest_reg, 0, model.preg_bits);
            let tmask = read_bit_mask(core_req_bits_tmask, model.num_lsu_lanes);
            let imm = read_packed_field(core_req_bits_imm, 0, model.arch_len);
            let byte_offsets = (0..model.num_lsu_lanes)
                .map(|lane| {
                    let base_address =
                        read_packed_field(core_req_bits_address, lane, model.arch_len);
                    model.byte_offset(model.lane_address(base_address, imm))
                })
                .collect();
            Some((token, op, dest_reg, tmask, byte_offsets))
        } else {
            None
        }
    } else {
        None
    };

    let global_response_fired = if let Some(tag) = visible_global_response_tag {
        if let Some(inflight) = model.global_inflight.get(&tag) {
            let resp_valid_mask = read_bit_mask(global_mem_resp_bits_valid, model.num_lsu_lanes);
            let needed_valid_mask = inflight.tmask & bit_mask_u64(model.num_lsu_lanes);
            let full_response = (resp_valid_mask & needed_valid_mask) == needed_valid_mask;
            if full_response && (!inflight.is_load() || core_resp_ready != 0) {
                Some(tag)
            } else {
                None
            }
        } else if model.global_retired_tags.contains_key(&tag) {
            None
        } else if model.global_store_inflight.contains_key(&tag)
            || (model.global_inflight.is_empty() && model.global_store_inflight.is_empty())
        {
            Some(tag)
        } else {
            panic!("cyclotron LSU received unknown global response tag 0x{tag:x}");
        }
    } else {
        None
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

    if let Some(tag) = global_response_fired {
        model.commit_global_response(tag);
    }
    if let Some((token, op, dest_reg, tmask, byte_offsets)) = global_request_fired {
        model.commit_global_request(token, op, dest_reg, tmask, byte_offsets);
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
    fn accepts_only_global_core_requests_before_shared_memory() {
        let model = make_model();

        let global_load_word = model.reservation_token(1, 0, MEM_OP_LOAD_WORD).unwrap();
        let global_store_word = model.reservation_token(1, 0, MEM_OP_STORE_WORD).unwrap();
        let shared_load_word = model.reservation_token(1, 1, MEM_OP_LOAD_WORD).unwrap();

        for op in MEM_OP_LOAD_BYTE..=MEM_OP_STORE_WORD {
            assert!(model.supports_global_request(global_load_word, op));
        }
        assert!(model.supports_global_request(global_store_word, MEM_OP_STORE_WORD));
        assert!(!model.supports_global_request(shared_load_word, MEM_OP_LOAD_WORD));
        assert!(!model.supports_global_request(global_load_word, 8));
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
    fn tracks_global_queue_empty_with_inflight_request() {
        let mut model = make_model();

        assert!(model.global_queues_empty(false));
        assert!(!model.global_queues_empty(true));

        let token = model.reservation_token(0, 0, MEM_OP_LOAD_WORD).unwrap();
        model.commit_global_request(token, MEM_OP_LOAD_WORD, 3, 0xffff, zero_byte_offsets());
        assert!(!model.global_queues_empty(false));

        model.commit_global_response(model.mem_tag(token));
        assert!(model.global_queues_empty(false));
    }

    #[test]
    fn tracks_store_inflight_by_source_tag() {
        let mut model = make_model();
        let token = model.reservation_token(0, 0, MEM_OP_STORE_WORD).unwrap();
        let tag = model.mem_tag(token);

        model.commit_global_request(token, MEM_OP_STORE_WORD, 0, 1, zero_byte_offsets());
        assert!(model.global_source_busy(tag));
        assert!(!model.global_queues_empty(false));

        model.commit_global_response(tag);
        assert!(!model.global_source_busy(tag));
        assert!(model.global_queues_empty(false));
    }

    #[test]
    fn moves_reservation_debug_id_into_global_inflight_metadata() {
        let mut model = make_model();

        let token = model.reservation_token(3, 0, MEM_OP_LOAD_WORD).unwrap();
        model.commit_reservation(3, 0, MEM_OP_LOAD_WORD, 1);
        model.commit_global_request(token, MEM_OP_LOAD_WORD, 7, 0xaaaa, zero_byte_offsets());

        let metadata = model.global_inflight.get(&model.mem_tag(token)).unwrap();
        assert_eq!(
            metadata,
            &GlobalInflight {
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
        model.commit_global_request(
            retired_token,
            MEM_OP_LOAD_WORD,
            3,
            0xffff,
            zero_byte_offsets(),
        );
        model.commit_global_response(retired_tag);

        let active_token = model.reservation_token(1, 0, MEM_OP_LOAD_WORD).unwrap();
        let active_tag = model.mem_tag(active_token);
        model.commit_global_request(
            active_token,
            MEM_OP_LOAD_WORD,
            4,
            0xffff,
            zero_byte_offsets(),
        );

        model.commit_global_response(retired_tag);
        assert!(model.global_inflight.contains_key(&active_tag));
    }
}
