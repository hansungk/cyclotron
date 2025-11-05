use crate::dpi::CELL;
use crate::muon::decode::{DecodedInst, MicroOp};
use log::debug;
use crate::base::module::IsModule;
use crate::timeq::module_now;

#[no_mangle]
pub unsafe fn cyclotron_imem_rs(
    imem_req_ready_ptr: *mut u8,
    imem_req_valid: u8,
    imem_req_bits_store: u8,
    imem_req_bits_address: u32,
    imem_req_bits_size: u8,
    imem_req_bits_tag: u8,
    _imem_req_bits_data: u64,
    _imem_req_bits_mask: u8,
    imem_resp_ready: u8,
    imem_resp_valid_ptr: *mut u8,
    imem_resp_bits_tag_ptr: *mut u8,
    imem_resp_bits_data_ptr: *mut u64,
) {
    let mut context_guard = CELL.write().unwrap();
    let context = context_guard.as_mut().expect("DPI context not initialized!");
    let sim = &mut context.sim_be;
    let cluster = &mut sim.top.clusters[0];
    let core = &mut cluster.cores[0];

    let imem_req_ready = unsafe { imem_req_ready_ptr.as_mut().expect("pointer was null") };
    let imem_resp_valid = unsafe { imem_resp_valid_ptr.as_mut().expect("pointer was null") };
    let imem_resp_bits_tag = unsafe { imem_resp_bits_tag_ptr.as_mut().expect("pointer was null") };
    let imem_resp_bits_data = unsafe { imem_resp_bits_data_ptr.as_mut().expect("pointer was null") };

    *imem_req_ready = imem_resp_ready;

    // let gmem = sim.top.gmem.clone().write().expect("gmem poisoned");
    if imem_req_valid != 0 {
        assert_eq!(imem_req_bits_store, 0, "imem is read only");
        assert_eq!(imem_req_bits_size, 3, "imem access size must be 8 bytes");
        *imem_resp_bits_data = core.warps[0].fetch(imem_req_bits_address);
    }

    *imem_resp_valid = imem_req_valid;
    *imem_resp_bits_tag = imem_req_bits_tag;
}

/// Issue a decoded instruction bundle to the backend model, and get the writeback bundle back.
#[no_mangle]
pub unsafe fn cyclotron_backend_rs(
    issue_valid: u8,
    issue_warp_id: u8,
    issue_pc: u32,
    issue_op: u8,
    issue_opext: u8,
    issue_f3: u8,
    issue_rd_addr: u8,
    issue_rs1_addr: u8,
    issue_rs2_addr: u8,
    issue_rs3_addr: u8,
    _issue_rs1_data_ptr: *const u32,
    _issue_rs2_data_ptr: *const u32,
    _issue_rs3_data_ptr: *const u32,
    issue_f7: u8,
    issue_imm32: u32,
    issue_imm24: u32,
    issue_csr_imm: u8,
    _issue_pred_ptr: *const u32,
    issue_tmask: u32,
    issue_raw_inst: u64,
    writeback_valid_ptr: *mut u8,
    writeback_pc_ptr: *mut u32,
    writeback_tmask_ptr: *mut u32,
    writeback_wid_ptr: *mut u8,
    _writeback_rd_addr_ptr: *mut u8,
    _writeback_rd_data_ptr: *mut u32,
    writeback_set_pc_valid_ptr: *mut u8,
    writeback_set_pc_ptr: *mut u32,
    writeback_set_tmask_valid_ptr: *mut u8,
    writeback_set_tmask_ptr: *mut u32,
    writeback_wspawn_valid_ptr: *mut u8,
    writeback_wspawn_count_ptr: *mut u32,
    writeback_wspawn_pc_ptr: *mut u32,
    writeback_ipdom_valid_ptr: *mut u8,
    writeback_ipdom_restored_mask_ptr: *mut u32,
    writeback_ipdom_else_mask_ptr: *mut u32,
    writeback_ipdom_else_pc_ptr: *mut u32,
    finished_ptr: *mut u8,
) {
    let mut context_guard = CELL.write().unwrap();
    let context = context_guard.as_mut().expect("DPI context not initialized!");
    let sim = &mut context.sim_be;
    let cluster = &mut sim.top.clusters[0];
    let core = &mut cluster.cores[0];
    // let config = *core.conf();
    let neutrino = &mut cluster.neutrino;

    // let issue_rs1_data = unsafe { std::slice::from_raw_parts(issue_rs1_data_ptr, config.num_lanes) };
    // let issue_rs2_data = unsafe { std::slice::from_raw_parts(issue_rs2_data_ptr, config.num_lanes) };
    // let issue_rs3_data = unsafe { std::slice::from_raw_parts(issue_rs3_data_ptr, config.num_lanes) };
    // predicates not used for now
    // let _issue_pred = unsafe { std::slice::from_raw_parts(issue_pred_ptr, config.num_lanes) };
    let writeback_valid = unsafe { writeback_valid_ptr.as_mut().expect("pointer was null") };
    let writeback_pc = unsafe { writeback_pc_ptr.as_mut().expect("pointer was null") };
    let writeback_tmask = unsafe { writeback_tmask_ptr.as_mut().expect("pointer was null") };
    let writeback_wid = unsafe { writeback_wid_ptr.as_mut().expect("pointer was null") };
    // let writeback_rd_addr = unsafe { writeback_rd_addr_ptr.as_mut().expect("pointer was null") };
    // let writeback_rd_data = unsafe { std::slice::from_raw_parts_mut(writeback_rd_data_ptr, config.num_lanes) };
    let writeback_set_pc_valid = unsafe { writeback_set_pc_valid_ptr.as_mut().expect("pointer was null") };
    let writeback_set_pc = unsafe { writeback_set_pc_ptr.as_mut().expect("pointer was null") };
    let writeback_set_tmask_valid = unsafe { writeback_set_tmask_valid_ptr.as_mut().expect("pointer was null") };
    let writeback_set_tmask = unsafe { writeback_set_tmask_ptr.as_mut().expect("pointer was null") };
    let writeback_wspawn_valid = unsafe { writeback_wspawn_valid_ptr.as_mut().expect("pointer was null") };
    let writeback_wspawn_count = unsafe { writeback_wspawn_count_ptr.as_mut().expect("pointer was null") };
    let writeback_wspawn_pc = unsafe { writeback_wspawn_pc_ptr.as_mut().expect("pointer was null") };
    let writeback_ipdom_valid = unsafe { writeback_ipdom_valid_ptr.as_mut().expect("pointer was null") };
    let writeback_ipdom_restored_mask = unsafe { writeback_ipdom_restored_mask_ptr.as_mut().expect("pointer was null") };
    let writeback_ipdom_else_mask = unsafe { writeback_ipdom_else_mask_ptr.as_mut().expect("pointer was null") };
    let writeback_ipdom_else_pc = unsafe { writeback_ipdom_else_pc_ptr.as_mut().expect("pointer was null") };
    let finished = unsafe { finished_ptr.as_mut().expect("pointer was null") };

    if issue_valid != 1 {
        // if no issue, tie off writeback and exit early
        *writeback_valid = 0u8;
        return;
    }

    // let rf = &core.warps[issue_warp_id as usize].base.state.reg_file;
    // debug!("rs1 {:#?}", rf.iter().map(|r| Some(r.read_gpr(issue_rs1_addr))).collect::<Vec<_>>());
    let decoded = DecodedInst {
        opcode: issue_op,
        opext: issue_opext,
        rd_addr: issue_rd_addr,
        f3: issue_f3,
        rs1_addr: issue_rs1_addr,
        rs2_addr: issue_rs2_addr,
        rs3_addr: issue_rs3_addr,
        rs4_addr: 0, /* unused */
        // rs1_data: rf.iter().map(|r| Some(r.read_gpr(issue_rs1_addr))).collect::<Vec<_>>(),
        // rs2_data: rf.iter().map(|r| Some(r.read_gpr(issue_rs2_addr))).collect::<Vec<_>>(),
        // rs3_data: rf.iter().map(|r| Some(r.read_gpr(issue_rs3_addr))).collect::<Vec<_>>(),
        // rs4_data: rf.iter().map(|r| Some(r.read_gpr(0))).collect::<Vec<_>>(), /* unused */
        f7: issue_f7,
        imm32: issue_imm32,
        imm24: ((issue_imm24 << 8) as i32) >> 8,
        csr_imm: issue_csr_imm,
        pc: issue_pc,
        raw: issue_raw_inst,
    };

    core.warps[issue_warp_id as usize].state_mut().csr_file.iter_mut().for_each(|c| {
        // c.emu_access(0xcc3, schedule.active_warps);
        c.emu_access(0xcc4, issue_tmask);
    });

    debug!("issue warp id is {}", issue_warp_id);
    debug!("{}", decoded);

    let (scheduler, warps, shared_mem, timing_model) = core.parts_mut();
    let warp = &mut warps[issue_warp_id as usize];
    let now = module_now(scheduler);
    timing_model.tick(now, scheduler);

    let writeback = warp
        .backend(
            MicroOp {
                inst: decoded,
                tmask: issue_tmask,
            },
            scheduler,
            neutrino,
            shared_mem,
            timing_model,
            now,
        )
        .expect("Warp backend panicked");

    if let Some(writeback) = writeback {
        *writeback_valid = 1u8;
        *writeback_pc = writeback.inst.pc;
        *writeback_tmask = writeback.tmask;
        *writeback_wid = issue_warp_id;
        *writeback_set_pc_valid = writeback.sched_wb.pc.is_some() as u8;
        *writeback_set_pc = writeback.sched_wb.pc.unwrap_or(0);
        *writeback_set_tmask_valid = writeback.sched_wb.tmask.is_some() as u8;
        *writeback_set_tmask = writeback.sched_wb.tmask.unwrap_or(0);
        *writeback_wspawn_valid = writeback.sched_wb.wspawn_pc_count.is_some() as u8;
        *writeback_wspawn_count = writeback.sched_wb.wspawn_pc_count.unwrap_or((0, 0)).1;
        *writeback_wspawn_pc = writeback.sched_wb.wspawn_pc_count.unwrap_or((0, 0)).0;
        *writeback_ipdom_valid = writeback.sched_wb.ipdom_push.is_some() as u8;
        *writeback_ipdom_restored_mask = writeback
            .sched_wb
            .ipdom_push
            .map(|x| x.restored_mask)
            .unwrap_or(0);
        *writeback_ipdom_else_mask = writeback
            .sched_wb
            .ipdom_push
            .map(|x| x.else_mask)
            .unwrap_or(0);
        *writeback_ipdom_else_pc = writeback
            .sched_wb
            .ipdom_push
            .map(|x| x.else_pc)
            .unwrap_or(0);
        *finished = writeback.sched_wb.tohost.is_some() as u8;
    } else {
        *writeback_valid = 0u8;
        *finished = 0u8;
        return;
    }

    // for (data_pin, owb) in zip(writeback_rd_data, writeback.rd_data) {
    //     *data_pin = owb.unwrap_or(0);
    // }
}
