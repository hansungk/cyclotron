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
    imem_resp_bits_tag: u64,
    imem_resp_bits_data: u64,
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

    let imem_req_valid = unsafe { imem_req_valid_ptr.as_mut().expect("pointer was null") };
    let imem_req_bits_address = unsafe {
        imem_req_bits_address_ptr
            .as_mut()
            .expect("pointer was null")
    };
    let imem_req_bits_tag = unsafe { imem_req_bits_tag_ptr.as_mut().expect("pointer was null") };
    let imem_resp_ready = unsafe { imem_resp_ready_ptr.as_mut().expect("pointer was null") };
    let finished = unsafe { finished_ptr.as_mut().expect("pointer was null") };

    *imem_req_valid = 0u8;
    *imem_resp_ready = 1u8; // cyclotron never stalls

    // A difference between Cyclotron-as-a-Tile and the ISA model is that the ISA model advances
    // all active warps for each cycle, whereas CaaT serializes them and advances one warp at a
    // cycle.  So here, CaaT needs to store the pipeline state & do simple warp scheduling to keep
    // track of individual warps' progress.

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

        // execute the fetched warp
        let inst: u64 = imem_resp_bits_data;
        let sched = pipe_context.next_schedule.unwrap();
        let uop = core.warps[sched.warp].frontend_nofetch(sched, inst);

        // clear sched bookkeep
        pipe_context.next_schedule = None;

        core.warps[sched.warp]
            .backend(
                uop,
                &mut core.scheduler,
                neutrino,
                &mut core.shared_mem, // TODO: sharedmem
            )
            .expect("cyclotron: backend() failed");

        // TODO: memory instruction
    }

    // before we tick the next warp's schedule, abort if imem is blocked
    if imem_req_ready == 0 {
        return;
    }

    // at this point, an instruction is retired; move to fetching the next instruction

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
