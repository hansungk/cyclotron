use std::os::raw::c_int;
use std::slice;

pub(super) struct CyclotronLsuModel {
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
        mem_op_bits: c_int,
        preg_bits: c_int,
        packet_bits: c_int,
        source_id_bits: c_int,
        per_lane_mask_bits: c_int,
        debug_id_port_bits: c_int,
    ) -> Self {
        Self {
            core_reservations_req_ready_words: dpi_words(num_warps),
            core_reservations_resp_valid_words: dpi_words(num_warps),
            core_reservations_resp_bits_token_words: dpi_words(num_warps * token_bits),
            core_resp_bits_warp_id_words: dpi_words(warp_id_bits),
            core_resp_bits_packet_words: dpi_words(packet_bits),
            core_resp_bits_tmask_words: dpi_words(num_lanes),
            core_resp_bits_dest_reg_words: dpi_words(preg_bits),
            core_resp_bits_writeback_data_words: dpi_words(num_lsu_lanes * arch_len),
            core_resp_bits_debug_id_words: dpi_words(debug_id_port_bits),
            mem_req_bits_tag_words: dpi_words(source_id_bits),
            mem_req_bits_op_words: dpi_words(mem_op_bits),
            mem_req_bits_address_words: dpi_words(num_lsu_lanes * arch_len),
            mem_req_bits_data_words: dpi_words(num_lsu_lanes * arch_len),
            mem_req_bits_mask_words: dpi_words(num_lsu_lanes * per_lane_mask_bits),
            mem_req_bits_tmask_words: dpi_words(num_lsu_lanes),
        }
    }

    fn reset(&mut self) {}
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

fn dpi_words(bits: c_int) -> usize {
    ((bits as usize) + 31) / 32
}

unsafe fn read_u32(value: *const u32) -> u32 {
    assert!(!value.is_null(), "cyclotron LSU scalar pointer is null");
    *value
}

unsafe fn zero_bit(value: *mut u8) {
    if let Some(value) = value.as_mut() {
        *value = 0;
    }
}

unsafe fn zero_words(value: *mut u32, words: usize) {
    if value.is_null() {
        return;
    }
    slice::from_raw_parts_mut(value, words).fill(0);
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
pub unsafe extern "C" fn cyclotron_lsu_eval_rs(
    cluster_id: *const u32,
    core_id: *const u32,
    _core_reservations_req_valid: *const u32,
    _core_reservations_req_bits_address_space: *const u32,
    _core_reservations_req_bits_op: *const u32,
    _core_reservations_req_bits_debug_id: *const u32,
    _core_req_valid: u8,
    _core_req_bits_token: *const u32,
    _core_req_bits_op: *const u32,
    _core_req_bits_tmask: *const u32,
    _core_req_bits_address: *const u32,
    _core_req_bits_imm: *const u32,
    _core_req_bits_dest_reg: *const u32,
    _core_req_bits_store_data: *const u32,
    _core_resp_ready: u8,
    _global_mem_req_ready: u8,
    _global_mem_resp_valid: u8,
    _global_mem_resp_bits_tag: *const u32,
    _global_mem_resp_bits_valid: *const u32,
    _global_mem_resp_bits_data: *const u32,
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
    zero_bit(shared_queues_empty);
    zero_bit(global_queues_empty);
}

#[no_mangle]
pub unsafe extern "C" fn cyclotron_lsu_commit_rs(
    cluster_id: *const u32,
    core_id: *const u32,
    _core_reservations_req_valid: *const u32,
    _core_reservations_req_bits_address_space: *const u32,
    _core_reservations_req_bits_op: *const u32,
    _core_reservations_req_bits_debug_id: *const u32,
    _core_req_valid: u8,
    _core_req_bits_token: *const u32,
    _core_req_bits_op: *const u32,
    _core_req_bits_tmask: *const u32,
    _core_req_bits_address: *const u32,
    _core_req_bits_imm: *const u32,
    _core_req_bits_dest_reg: *const u32,
    _core_req_bits_store_data: *const u32,
    _core_resp_ready: u8,
    _global_mem_req_ready: u8,
    _global_mem_resp_valid: u8,
    _global_mem_resp_bits_tag: *const u32,
    _global_mem_resp_bits_valid: *const u32,
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
    let _model = context.lsu_model_mut(cluster_id, core_id);
}
