use crate::base::behavior::*;
use crate::dpi::CELL;
use crate::muon::scheduler::Schedule;
use crate::muon::warp::Writeback;
use std::slice::{from_raw_parts, from_raw_parts_mut};

/// Holds the current control state of the Cyclotron pipeline, e.g. what's the
/// next warp/PC to schedule, are we waiting for a IMEM/DMEM response to come
/// back, etc.
#[derive(Default)]
pub struct CyclotronPipelineContext {
    last_warp: usize,
    next_schedule: Option<Schedule>,
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
    let dmem_resp_ready = unsafe { from_raw_parts_mut(dmem_resp_ready_vec, num_lanes) };
    let finished = unsafe { finished_ptr.as_mut().expect("pointer was null") };

    *imem_req_valid = 0u8;
    *imem_resp_ready = 1u8; // cyclotron never stalls

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

    // wait for previous fetch imem req to come back
    if pipe_context.next_schedule.is_some() {
        if imem_resp_valid == 0 {
            return;
        }

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

        // warp.backend(
        //         uop,
        //         &mut core.scheduler,
        //         neutrino,
        //         &mut core.shared_mem, // TODO: sharedmem
        //     )
        //     .expect("cyclotron: backend() failed");

        // collector/EX(alu, fpu)
        let ex_writeback = warp
            .backend_beforemem(uop, &mut core.scheduler, neutrino)
            .expect("cyclotron: execute() failed");

        // MEM
        // abort if downstream not ready
        // TODO: how to handle replay?
        let dmem_all_ready = dmem_req_ready.iter().all(|ready| *ready == 1u8);
        if dmem_all_ready {
            let mem_reqs = ex_writeback.mem_req;
            for (l, req) in mem_reqs.iter().enumerate() {
                match req {
                    Some(req) => {
                        dmem_req_valid[l] = 1u8;
                        dmem_req_bits_address[l] = req.addr;
                        // single outstanding req
                        dmem_req_bits_tag[l] = 0;
                        dmem_req_bits_size[l] = 2; // 32-bit word
                        // TODO: data, tag
                    },
                    None => {
                        dmem_req_valid[l] = 0u8;
                        dmem_req_bits_store[l] = 0;
                        dmem_req_bits_address[l] = 0;
                        dmem_req_bits_tag[l] = 0;
                        dmem_req_bits_size[l] = 2; // 32-bit word
                        // TODO: data, tag
                    }
                }
            }
        }

        // TODO: fake writeback
        let writeback = Writeback {
            inst: ex_writeback.inst,
            tmask: ex_writeback.tmask,
            rd_addr: ex_writeback.rd_addr,
            rd_data: ex_writeback.rd_data,
            sched_wb: ex_writeback.sched_wb,
        };
        warp.writeback(&writeback);

        // instruction is retired; clear next-cycle schedule
        pipe_context.next_schedule = None;
    }

    // move to fetching the next instruction

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
        pipe_context.last_warp = warp;
        pipe_context.next_schedule = sched;
    }

    *finished = sim.finished() as u8;
}
