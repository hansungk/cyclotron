// #![allow(dead_code, unreachable_code)]
use crate::base::behavior::*;
use crate::muon::decode::IssuedInst;
use crate::sim::top::Sim;
use std::iter::zip;
use std::path::PathBuf;
use std::sync::RwLock;

struct Context {
    sim_isa: Sim, // cyclotron instance for the ISA model
    sim_be: Sim,  // cyclotron instance for the backend model
}

/// Global singleton to maintain simulator context across independent DPI calls.
static CELL: RwLock<Option<Context>> = RwLock::new(None);

pub fn assert_single_core(sim: &Sim) {
    assert!(sim.top.clusters.len() == 1, "currently assumes model has 1 cluster and 1 core");
    assert!(sim.top.clusters[0].cores.len() == 1, "currently assumes model has 1 cluster and 1 core");
}

#[no_mangle]
/// Entry point to the DPI interface.  This must be called from Verilog once at the start in an
/// initial block.
pub fn cyclotron_init_rs() {
    let toml_path = PathBuf::from("config.toml");
    let toml_string = crate::ui::read_toml(&toml_path);

    // make separate sim instances for the golden ISA model and the backend model to prevent
    // double-execution on the same GMEM
    let sim_isa = crate::ui::make_sim(&toml_string, None);
    let sim_be = crate::ui::make_sim(&toml_string, None);
    assert_single_core(&sim_isa);
    assert_single_core(&sim_be);

    println!("cyclotron_init_rs: created sim object from {}", toml_path.display());

    let mut c = Context {
        sim_isa,
        sim_be,
    };
    c.sim_isa.top.reset();
    c.sim_be.top.reset();

    let mut context = CELL.write().unwrap();
    if context.as_ref().is_some() {
        panic!("DPI context already initialized!");
    }
    *context = Some(c);
}

#[no_mangle]
/// Get un-decoded instruction bits from the instruction trace.
pub fn cyclotron_fetch_rs(
    fetch_pc: u32,
    fetch_warp: u32,
    inst_ptr: *mut u64,
) {
    let mut context_guard = CELL.write().unwrap();
    let context = context_guard.as_mut().expect("DPI context not initialized!");
    let sim = &mut context.sim_isa;
    let core = &mut sim.top.clusters[0].cores[0];
    let inst = core.fetch(fetch_warp, fetch_pc);

    let inst_slice = unsafe { std::slice::from_raw_parts_mut(inst_ptr, 1) };
    inst_slice[0] = inst;
}

#[no_mangle]
/// Get a decoded instruction bundle from the instruction trace.  Also advances the ISA model by a
/// single tick.
/// SAFETY: All signals are arrays of size num_warps.
pub unsafe fn cyclotron_get_trace_rs(
    ready_ptr: *const u8,
    valid_ptr: *mut u8,
    pc_ptr: *mut u32,
    op_ptr: *mut u32,
    rd_ptr: *mut u32,
    finished_ptr: *mut u8,
) {
    let mut context_guard = CELL.write().unwrap();
    let context = context_guard.as_mut().expect("DPI context not initialized!");
    let sim = &mut context.sim_isa;

    // advance simulation to populate inst trace buffers
    sim.tick();

    let core = &mut sim.top.clusters[0].cores[0];
    let config = *core.conf();
    let warp_insts: Vec<_> = (0..config.num_warps).map(|w| {
        core.get_tracer().peek(w).cloned() // @perf: expensive?
    }).collect();

    // SAFETY: precondition of function guarantees this is valid
    let ready = unsafe { std::slice::from_raw_parts(ready_ptr, config.num_warps) };
    let valid = unsafe { std::slice::from_raw_parts_mut(valid_ptr, config.num_warps) };
    let pc = unsafe { std::slice::from_raw_parts_mut(pc_ptr, config.num_warps) };
    let op = unsafe { std::slice::from_raw_parts_mut(op_ptr, config.num_warps) };
    let rd = unsafe { std::slice::from_raw_parts_mut(rd_ptr, config.num_warps) };
    let finished = unsafe { std::slice::from_raw_parts_mut(finished_ptr, 1) };

    for (w, maybe_inst) in warp_insts.iter().enumerate() {
        match maybe_inst {
            Some(inst) => {
                valid[w] = 1;
                pc[w] = inst.pc;
                op[w] = inst.opcode as u32;
                rd[w] = inst.rd_addr as u32;
            },
            None => {
                valid[w] = 0;
            }
        }
    }

    // debug
    for maybe_inst in warp_insts.iter() {
        match maybe_inst {
            Some(inst) => { println!("trace: {}", inst); },
            _ => (),
        }
    }

    // consume if RTL ready was true
    // this has to happen after the pin drive above so that the consumed line is not lost
    zip(warp_insts, ready).enumerate().for_each(|(w, (inst, rdy))| {
        if inst.is_some() && *rdy == 1 {
            core.get_tracer().consume(w);
        }
    });

    finished[0] = sim.finished() as u8;
}

mod mem_model;
mod backend_model;

#[no_mangle]
/// Issue a decoded instruction bundle to the backend model, and get the writeback bundle back.
pub fn cyclotron_backend_rs(
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
    issue_rs1_data_ptr: *const u32,
    issue_rs2_data_ptr: *const u32,
    issue_rs3_data_ptr: *const u32,
    issue_f7: u8,
    issue_imm32: u32,
    issue_imm24: u32,
    issue_pred_ptr: *const u32,
    issue_tmask: u32,
    issue_raw_inst: u64,
    writeback_valid_ptr: *mut u8,
    writeback_tmask_ptr: *mut u32,
    writeback_rd_addr_ptr: *mut u8,
    writeback_rd_data_ptr: *mut u32,
) {
    let mut context_guard = CELL.write().unwrap();
    let context = context_guard.as_mut().expect("DPI context not initialized!");
    let sim = &mut context.sim_be;
    let cluster = &mut sim.top.clusters[0];
    let core = &mut cluster.cores[0];
    let config = *core.conf();
    let neutrino = &mut cluster.neutrino;

    let issue_rs1_data = unsafe { std::slice::from_raw_parts(issue_rs1_data_ptr, config.num_lanes) };
    let issue_rs2_data = unsafe { std::slice::from_raw_parts(issue_rs2_data_ptr, config.num_lanes) };
    let issue_rs3_data = unsafe { std::slice::from_raw_parts(issue_rs3_data_ptr, config.num_lanes) };
    // predicates not used for now
    let _issue_pred = unsafe { std::slice::from_raw_parts(issue_pred_ptr, config.num_lanes) };
    let writeback_valid = unsafe { writeback_valid_ptr.as_mut().expect("pointer was null") };
    let writeback_tmask = unsafe { writeback_tmask_ptr.as_mut().expect("pointer was null") };
    let writeback_rd_addr = unsafe { writeback_rd_addr_ptr.as_mut().expect("pointer was null") };
    let writeback_rd_data = unsafe { std::slice::from_raw_parts_mut(writeback_rd_data_ptr, config.num_lanes) };

    let issue_rs1_data_vec: Vec<_> = issue_rs1_data.iter().map(|u| { Some(*u) }).collect();
    let issue_rs2_data_vec: Vec<_> = issue_rs2_data.iter().map(|u| { Some(*u) }).collect();
    let issue_rs3_data_vec: Vec<_> = issue_rs3_data.iter().map(|u| { Some(*u) }).collect();
    let issue_rs4_data_vec: Vec<_> = issue_rs3_data.iter().map(|_| { Some(0 /*unused*/) }).collect();

    if issue_valid != 1 {
        // if no issue, tie off writeback and exit early
        *writeback_valid = 0 as u8;
        return;
    }

    let issued = IssuedInst {
        opcode: issue_op,
        opext: issue_opext,
        rd_addr: issue_rd_addr,
        f3: issue_f3,
        rs1_addr: issue_rs1_addr,
        rs2_addr: issue_rs2_addr,
        rs3_addr: issue_rs3_addr,
        rs4_addr: 0 /* unused */,
        rs1_data: issue_rs1_data_vec,
        rs2_data: issue_rs2_data_vec,
        rs3_data: issue_rs3_data_vec,
        rs4_data: issue_rs4_data_vec,
        f7: issue_f7,
        imm32: issue_imm32,
        imm24: issue_imm24 as i32,
        pc: issue_pc,
        raw: issue_raw_inst,
    };

    let writeback = core.execute(issue_warp_id.into(), issued, issue_tmask, neutrino);
    *writeback_valid = 1 as u8;
    *writeback_tmask = writeback.tmask;
    *writeback_rd_addr = writeback.rd_addr;
    for (data_pin, owb) in zip(writeback_rd_data, writeback.rd_data) {
        *data_pin = owb.unwrap_or(0);
    }
}
