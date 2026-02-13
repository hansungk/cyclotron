use crate::base::behavior::*;
use crate::dpi::CELL;
use crate::muon::scheduler::Schedule;
use crate::muon::warp::{ExWriteback, MemResponse};
use std::iter::zip;
use std::slice::{from_raw_parts, from_raw_parts_mut};

/// Holds the current control state of the Cyclotron pipeline, e.g. what's the
/// next warp/PC to schedule, are we waiting for a IMEM/DMEM response to come
/// back, etc.
pub struct PipelineContext {
    last_warp: usize,
    next_schedule: Option<Schedule>,
    state: PipelineState,
    pending_ex_writeback: Option<ExWriteback>,
    staged_mem_resps: Vec<Option<MemResponse>>,
}

#[derive(PartialEq, Debug)]
enum PipelineState {
    Execute,
    Mem,
    ScheduleNext,
}

impl PipelineContext {
    pub fn new(num_lanes: usize) -> Self {
        PipelineContext {
            last_warp: 0,
            next_schedule: None,
            state: PipelineState::ScheduleNext,
            pending_ex_writeback: None,
            staged_mem_resps: vec![None; num_lanes],
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn cyclotron_tile_tick_rs(
    imem_req_valid_ptr: *mut u8,
    imem_req_ready: u8,
    imem_req_bits_address_ptr: *mut u32,
    imem_req_bits_tag_ptr: *mut u64,
    imem_resp_ready_ptr: *mut u8,
    imem_resp_valid: u8,
    imem_resp_bits_tag: u64,
    imem_resp_bits_data: u64,
    dmem_req_valid_vec: *mut u8,
    dmem_req_ready_vec: *const u8,
    dmem_req_bits_store_vec: *mut u8,
    dmem_req_bits_address_vec: *mut u32,
    dmem_req_bits_size_vec: *mut u8,
    dmem_req_bits_tag_vec: *mut u32,
    dmem_req_bits_data_vec: *mut u32,
    dmem_req_bits_mask_vec: *mut u8,
    dmem_resp_ready_vec: *mut u8,
    dmem_resp_valid_vec: *const u8,
    dmem_resp_bits_tag_vec: *const u32,
    dmem_resp_bits_data_vec: *const u32,
    finished_ptr: *mut u8,
) {
    let mut context_guard = CELL.write().unwrap();
    let context = context_guard
        .as_mut()
        .expect("DPI context not initialized!");
    let pipe_context = &mut context.pipeline_context;
    // TODO: support cluster/coreID != 0
    let sim = &mut context.sim_isa;
    let top = &mut sim.top;
    let config = top.clusters[0].cores[0].conf();
    let num_warps = config.num_warps;
    let num_lanes = config.num_lanes;

    let imem_req_valid = unsafe { imem_req_valid_ptr.as_mut().expect("pointer was null") };
    let imem_req_bits_address = unsafe {
        imem_req_bits_address_ptr
            .as_mut()
            .expect("pointer was null")
    };
    let imem_req_bits_tag = unsafe { imem_req_bits_tag_ptr.as_mut().expect("pointer was null") };
    let imem_resp_ready = unsafe { imem_resp_ready_ptr.as_mut().expect("pointer was null") };
    let dmem_req_ready = unsafe { from_raw_parts(dmem_req_ready_vec, num_lanes) };
    let dmem_req_valid = unsafe { from_raw_parts_mut(dmem_req_valid_vec, num_lanes) };
    let dmem_req_bits_store = unsafe { from_raw_parts_mut(dmem_req_bits_store_vec, num_lanes) };
    let dmem_req_bits_address = unsafe { from_raw_parts_mut(dmem_req_bits_address_vec, num_lanes) };
    let dmem_req_bits_size = unsafe { from_raw_parts_mut(dmem_req_bits_size_vec, num_lanes) };
    let dmem_req_bits_tag = unsafe { from_raw_parts_mut(dmem_req_bits_tag_vec, num_lanes) };
    let dmem_req_bits_data = unsafe { from_raw_parts_mut(dmem_req_bits_data_vec, num_lanes) };
    let dmem_req_bits_mask = unsafe { from_raw_parts_mut(dmem_req_bits_mask_vec, num_lanes) };
    let dmem_resp_ready = unsafe { from_raw_parts_mut(dmem_resp_ready_vec, num_lanes) };
    let dmem_resp_valid = unsafe { from_raw_parts(dmem_resp_valid_vec, num_lanes) };
    let dmem_resp_bits_tag = unsafe { from_raw_parts(dmem_resp_bits_tag_vec, num_lanes) };
    let dmem_resp_bits_data = unsafe { from_raw_parts(dmem_resp_bits_data_vec, num_lanes) };
    let finished = unsafe { finished_ptr.as_mut().expect("pointer was null") };

    // pin to default
    *imem_req_valid = 0;
    *imem_req_bits_address = 0;
    *imem_req_bits_tag = 0;
    // cyclotron never stalls
    *imem_resp_ready = 1;
    for i in 0..num_lanes {
        dmem_req_valid[i] = 0;
        dmem_req_bits_store[i] = 0;
        dmem_req_bits_address[i] = 0;
        dmem_req_bits_size[i] = 0;
        dmem_req_bits_tag[i] = 0;
        dmem_req_bits_data[i] = 0;
        dmem_req_bits_mask[i] = 0;
        dmem_resp_ready[i] = 1;
    }

    // A difference between Cyclotron-as-a-Tile and the ISA model is that the ISA model advances
    // all active warps for each cycle, whereas CaaT serializes them and advances one warp at a
    // cycle.  So here, CaaT needs to store the pipeline state & do simple warp scheduling to keep
    // track of individual warps' progress.
    //
    // Pipelining scheme:
    //
    // I0(alu): F | DXMW
    // I1(mem):         F | DXM | W
    // I2(...):                    F |

    // start by scheduling threadblocks, if any
    top.schedule_clusters();

    let cluster = &mut top.clusters[0];
    let neutrino = &mut cluster.neutrino;
    let core = &mut cluster.cores[0];

    let mut warp_id = 0usize;

    // wait for previous fetch imem req to come back
    if pipe_context.state == PipelineState::Execute && imem_resp_valid != 0 {
        // note we are only sending out 1 outstanding imem req at a time
        if imem_resp_bits_tag != 0 {
            panic!(
                "cyclotron_tile_tick: unexpected tag in imem resp: {}",
                imem_resp_bits_tag
            );
        }

        // decode the fetched instruction
        let inst: u64 = imem_resp_bits_data;
        let sched = pipe_context.next_schedule.unwrap();
        let uop = core.warps[sched.warp].frontend_nofetch(sched, inst);
        let warp = &mut core.warps[sched.warp];

        // collector/EX(alu, fpu)
        let ex_writeback = warp
            .backend_beforemem(uop, &mut core.scheduler, neutrino)
            .expect("cyclotron: execute() failed");
        warp_id = sched.warp;

        // MEM
        // abort if downstream not ready
        let dmem_all_ready = dmem_req_ready.iter().all(|ready| *ready == 1u8);
        if dmem_all_ready {
            let mem_req = &ex_writeback.mem_req;
            for (i, req) in mem_req.iter().enumerate() {
                if let Some(req) = req {
                    dmem_req_valid[i] = 1u8;
                    dmem_req_bits_store[i] = 0; // TODO
                    dmem_req_bits_address[i] = req.addr;
                    // single outstanding req
                    dmem_req_bits_size[i] = 2; // 32-bit word
                    dmem_req_bits_tag[i] = 0;
                    dmem_req_bits_data[i] = 0; // TODO
                    dmem_req_bits_mask[i] = 0xf; // TODO
                }
            }
        } else {
            panic!("dmem is not ready!");
        }

        // instruction is fetched & executed
        pipe_context.state = PipelineState::Mem;
        pipe_context.pending_ex_writeback = Some(ex_writeback);
        pipe_context.next_schedule = None;
    }

    // MEM: handle response after stalling
    if pipe_context.state == PipelineState::Mem {
        let ex_wb = pipe_context.pending_ex_writeback.as_ref().expect("ex_writeback empty at MEM stage!");

        // per-lane responses may come back at different times; need to stage them so that the
        // individual resps don't get lost before all of them comes back
        let mem_req = &ex_wb.mem_req;
        for (i, req) in mem_req.iter().enumerate() {
            if dmem_resp_valid[i] != 1u8 {
                continue;
            }

            let req = req.as_ref().expect("dmem response on an invalid req!");
            assert!(dmem_resp_bits_tag[i] == 0, "dmem req and resp tag mismatch");
            let data = dmem_resp_bits_data[i].to_le_bytes();
            let resp = MemResponse {
                data: (!req.is_store).then_some(data),
                is_sext: req.is_sext,
            };
            // TODO: don't think staged_mem_resps is initialized to the correct length
            pipe_context.staged_mem_resps[i] = Some(resp);
        }

        let mut all_reqs_responded = true;
        for (req, resp) in zip(mem_req, &pipe_context.staged_mem_resps) {
            let this_req_responded = req.is_none() || resp.is_some();
            if !this_req_responded {
                all_reqs_responded = false;
                break;
            }
        }
        if all_reqs_responded {
            let warp = &mut core.warps[warp_id];
            let mem_resps = &pipe_context.staged_mem_resps;

            // MEM
            let writeback = warp.mem(ex_wb, &mut core.shared_mem, Some(mem_resps));

            // WB
            // TODO: this should run even if no mem went out
            warp.writeback(&writeback);

            pipe_context.state = PipelineState::ScheduleNext;
            pipe_context.pending_ex_writeback = None;
            for resp in &mut pipe_context.staged_mem_resps {
                *resp = None;
            }
        }
    }

    if pipe_context.state == PipelineState::ScheduleNext {
        // before we tick the next warp's schedule, abort if imem is blocked
        if imem_req_ready == 0 {
            return;
        }

        // warp fetch scheduling
        let mut warp = 0;
        let mut sched = None;
        for i in 0..num_warps {
            // round-robin
            warp = (pipe_context.last_warp + i + 1) % num_warps;
            sched = core.scheduler.schedule(warp);
            if sched.is_some() {
                break;
            }
        }

        if let Some(s) = sched {
            let pc = s.pc;
            *imem_req_valid = 1u8;
            *imem_req_bits_address = pc;
            *imem_req_bits_tag = 0;

            // update pipeline context for the next cycle
            pipe_context.state = PipelineState::Execute;
            pipe_context.last_warp = warp;
            pipe_context.next_schedule = sched;
        }
    }

    *finished = sim.finished() as u8;
}
