// #![allow(dead_code, unreachable_code)]
use crate::base::behavior::*;
use crate::muon::core::MuonCore;
use crate::sim::top::Sim;
use crate::sim::trace;
use std::iter::zip;
use std::path::PathBuf;
use std::sync::RwLock;

use env_logger::Builder;
use log::LevelFilter;

struct Context {
    sim_isa: Sim, // cyclotron instance for the ISA model
    sim_be: Sim,  // cyclotron instance for the backend model
}

/// Global singleton to maintain simulator context across independent DPI calls.
static CELL: RwLock<Option<Context>> = RwLock::new(None);

pub fn assert_single_core(sim: &Sim) {
    assert!(
        sim.top.clusters.len() == 1,
        "currently assumes model has 1 cluster and 1 core"
    );
    assert!(
        sim.top.clusters[0].cores.len() == 1,
        "currently assumes model has 1 cluster and 1 core"
    );
}

#[no_mangle]
/// Entry point to the DPI interface.  This must be called from Verilog once at the start in an
/// initial block.
pub fn cyclotron_init_rs() {
    let log_level = LevelFilter::Debug;

    Builder::new().filter_level(log_level).init();
    let toml_path = PathBuf::from("config.toml");
    let toml_string = crate::ui::read_toml(&toml_path);

    // make separate sim instances for the golden ISA model and the backend model to prevent
    // double-execution on the same GMEM
    let sim_isa = crate::ui::make_sim(&toml_string, None);
    let sim_be = crate::ui::make_sim(&toml_string, None);
    assert_single_core(&sim_isa);
    assert_single_core(&sim_be);

    println!("cyclotron_init_rs: created sim object from {}", toml_path.display());

    let mut c = Context { sim_isa, sim_be };
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
pub fn cyclotron_fetch_rs(fetch_pc: u32, fetch_warp: u32, inst_ptr: *mut u64) {
    let mut context_guard = CELL.write().unwrap();
    let context = context_guard.as_mut().expect("DPI context not initialized!");
    let sim = &mut context.sim_isa;
    let core = &mut sim.top.clusters[0].cores[0];
    let inst = core.fetch(fetch_warp, fetch_pc);

    let inst_slice = unsafe { std::slice::from_raw_parts_mut(inst_ptr, 1) };
    inst_slice[0] = inst;
}

fn peek_heads(c: &MuonCore, num_warps: usize) -> Vec<Option<trace::Line>> {
    (0..num_warps)
        .map(|w| c.get_tracer().peek(w).cloned())
        .collect::<Vec<_>>()
}

#[no_mangle]
/// Get a per-warp decoded instruction bundle from the instruction trace, and advance the ISA
/// model.  Models the fetch/decode frontend up until the ibuffers, and exposes per-warp ibuffer
/// head entries.
/// SAFETY: All signals are arrays of size num_warps.
pub unsafe fn cyclotron_frontend_rs(
    ibuf_ready_vec: *const u8,
    ibuf_valid_vec: *mut u8,
    ibuf_pc_vec: *mut u32,
    ibuf_op_vec: *mut u8,
    ibuf_opext_vec: *mut u8,
    ibuf_f3_vec: *mut u8,
    ibuf_rd_addr_vec: *mut u8,
    ibuf_rs1_addr_vec: *mut u8,
    ibuf_rs2_addr_vec: *mut u8,
    ibuf_rs3_addr_vec: *mut u8,
    ibuf_f7_vec: *mut u8,
    ibuf_imm32_vec: *mut u32,
    ibuf_imm24_vec: *mut u32,
    ibuf_csr_imm_vec: *mut u8,
    ibuf_tmask_vec: *mut u32,
    _ibuf_raw_vec: *mut u8,
    finished_ptr: *mut u8,
) {
    let mut context_guard = CELL.write().unwrap();
    let context = context_guard.as_mut().expect("DPI context not initialized!");
    let sim = &mut context.sim_isa;

    let core = &mut sim.top.clusters[0].cores[0];
    let config = core.conf().clone();

    // SAFETY: precondition of function guarantees this is valid
    let ready = unsafe { std::slice::from_raw_parts(ibuf_ready_vec, config.num_warps) };
    let valid = unsafe { std::slice::from_raw_parts_mut(ibuf_valid_vec, config.num_warps) };
    let tmask = unsafe { std::slice::from_raw_parts_mut(ibuf_tmask_vec, config.num_warps) };
    let pc = unsafe { std::slice::from_raw_parts_mut(ibuf_pc_vec, config.num_warps) };
    let op = unsafe { std::slice::from_raw_parts_mut(ibuf_op_vec, config.num_warps) };
    let opext = unsafe { std::slice::from_raw_parts_mut(ibuf_opext_vec, config.num_warps) };
    let f3 = unsafe { std::slice::from_raw_parts_mut(ibuf_f3_vec, config.num_warps) };
    let rd_addr = unsafe { std::slice::from_raw_parts_mut(ibuf_rd_addr_vec, config.num_warps) };
    let rs1_addr = unsafe { std::slice::from_raw_parts_mut(ibuf_rs1_addr_vec, config.num_warps) };
    let rs2_addr = unsafe { std::slice::from_raw_parts_mut(ibuf_rs2_addr_vec, config.num_warps) };
    let rs3_addr = unsafe { std::slice::from_raw_parts_mut(ibuf_rs3_addr_vec, config.num_warps) };
    let f7 = unsafe { std::slice::from_raw_parts_mut(ibuf_f7_vec, config.num_warps) };
    let imm32 = unsafe { std::slice::from_raw_parts_mut(ibuf_imm32_vec, config.num_warps) };
    let imm24 = unsafe { std::slice::from_raw_parts_mut(ibuf_imm24_vec, config.num_warps) };
    let csr_imm = unsafe { std::slice::from_raw_parts_mut(ibuf_csr_imm_vec, config.num_warps) };
    let finished = unsafe { finished_ptr.as_mut().expect("pointer was null") };

    // consume if RTL ready was true
    // this has to happen before the sim::tick() call below, so that it respects the
    // dequeue->enqueue data hazard
    let heads = peek_heads(core, config.num_warps);
    for (w, (o_line, rdy)) in zip(heads, ready).enumerate() {
        if o_line.is_some() && *rdy == 1 {
            core.get_tracer_mut().consume(w);
        }
    }

    // advance simulation to populate the tracer buffers
    sim.tick();

    // peek again and expose the new head to the RTL
    let core = &mut sim.top.clusters[0].cores[0];
    let new_heads = peek_heads(core, config.num_warps);
    for (w, o_line) in new_heads.iter().enumerate() {
        match o_line {
            Some(line) => {
                valid[w] = 1;
                tmask[w] = line.tmask;
                pc[w] = line.pc;
                op[w] = line.opcode;
                opext[w] = line.opext;
                f3[w] = line.f3;
                rd_addr[w] = line.rd_addr;
                rs1_addr[w] = line.rs1_addr;
                rs2_addr[w] = line.rs2_addr;
                rs3_addr[w] = line.rs3_addr;
                f7[w] = line.f7;
                imm32[w] = line.imm32;
                imm24[w] = line.imm24 as u32;
                csr_imm[w] = line.csr_imm;
            }
            None => {
                valid[w] = 0;
            }
        }
    }

    // debug
    if false {
        for maybe_inst in new_heads.iter() {
            match maybe_inst {
                Some(inst) => {
                    println!("trace: {}", inst);
                }
                _ => (),
            }
        }
    }

    *finished = sim.finished() as u8;
}

mod backend_model;
mod mem_model;
