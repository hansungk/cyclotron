use crate::base::behavior::*;
use crate::dpi::CELL;
use crate::muon::scheduler::Schedule;

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
    _imem_resp_bits_tag: u64,
    _imem_resp_bits_data: u64,
    _dmem_req_valid_vec: *mut u8,
    _dmem_req_ready_vec: *const u8,
    _dmem_req_bits_store_vec: *mut u8,
    _dmem_req_bits_address_vec: *mut u32,
    _dmem_req_bits_size_vec: *mut u8,
    _dmem_req_bits_tag_vec: *mut u32,
    _dmem_req_bits_data_vec: *mut u32,
    _dmem_req_bits_mask_vec: *mut u8,
    _dmem_resp_ready_vec: *mut u8,
    _dmem_resp_valid_vec: *const u8,
    _dmem_resp_bits_tag_vec: *const u32,
    _dmem_resp_bits_data_vec: *const u32,
    _finished_ptr: *mut u8,
) {
    let mut context_guard = CELL.write().unwrap();
    let context = context_guard
        .as_mut()
        .expect("DPI context not initialized!");
    let pipe_context = &mut context.pipeline_context;
    // TODO: support cluster/coreID != 0
    let sim = &mut context.sim_isa;
    let top = &mut sim.top;

    let imem_req_valid = unsafe { imem_req_valid_ptr.as_mut().expect("pointer was null") };
    let imem_req_bits_address = unsafe {
        imem_req_bits_address_ptr
            .as_mut()
            .expect("pointer was null")
    };
    let imem_req_bits_tag = unsafe { imem_req_bits_tag_ptr.as_mut().expect("pointer was null") };
    let imem_resp_ready = unsafe { imem_resp_ready_ptr.as_mut().expect("pointer was null") };

    *imem_req_valid = 0u8;
    *imem_resp_ready = 1u8; // cyclotron never stalls

    // A difference between Cyclotron-as-a-Tile and the ISA model is that the
    // ISA model advances all active warps for each cycle, whereas CaaT
    // serializes them and advances one warp at a cycle.  So CaaT needs a simple
    // warp scheduling logic & the pipeline context to keep track of the
    // individual warps' progress.

    // start by scheduling threadblocks, if any
    top.schedule_clusters();

    let core = &mut top.clusters[0].cores[0];
    let config = core.conf().clone();
    let num_warps = config.num_warps;

    // wait for previous fetch imem req to come back
    // note we are only sending out 1 outstanding imem req at a time
    if pipe_context.next_schedule.is_some() {
        if imem_resp_valid == 0 {
            return;
        }

        println!("cyclotron_tick: got fetch back");
    }

    // before we schedule & fetch the next warp, abort if imem is not ready
    if imem_req_ready == 0 {
        return;
    }

    // simple round-robin warp scheduling
    let mut warp = 0;
    let mut sched = None;
    for i in 0..num_warps {
        warp = (pipe_context.last_warp + i + 1) % num_warps;
        sched = core.scheduler.schedule(warp);
        if sched.is_some() {
            break;
        }
    }
    if sched.is_none() {
        // no active warp
        return;
    }

    let pc = sched.unwrap().pc;
    *imem_req_valid = 1u8;
    *imem_req_bits_address = pc;
    *imem_req_bits_tag = 0;

    // update pipeline context for the next cycle
    pipe_context.last_warp = warp;
    pipe_context.next_schedule = sched;

    println!("cyclotron_tick: scheduled warp:{} pc:{:x}", warp, pc);
}
