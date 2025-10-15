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

#[no_mangle]
/// Get a per-warp decoded instruction bundle from the instruction trace, and advance the ISA
/// model. This supplies instructions from all warps because the issue stage peeks on every warp's
/// instruction buffer head.
/// SAFETY: All signals are arrays of size num_warps.
pub unsafe fn cyclotron_get_trace_rs(
    ready_ptr: *const u8,
    valid_ptr: *mut u8,
    tmask_ptr: *mut u32,
    pc_ptr: *mut u32,
    opcode_ptr: *mut u32,
    f3_ptr: *mut u8,
    rd_addr_ptr: *mut u8,
    rs1_addr_ptr: *mut u8,
    rs2_addr_ptr: *mut u8,
    rs3_addr_ptr: *mut u8,
    f7_ptr: *mut u8,
    imm32_ptr: *mut u32,
    imm24_ptr: *mut u32,
    csr_imm_ptr: *mut u8,
    // raw_inst: *mut u64,
    finished_ptr: *mut u8,
) {
    let mut context_guard = CELL.write().unwrap();
    let context = context_guard.as_mut().expect("DPI context not initialized!");
    let sim = &mut context.sim_isa;

    // advance simulation to populate inst trace buffers
    sim.tick();

    let core = &mut sim.top.clusters[0].cores[0];
    let config = *core.conf();
    let warp_insts: Vec<_> = (0..config.num_warps)
        .map(|w| {
            core.get_tracer().peek(w).cloned() // @perf: expensive?
        })
        .collect();

    // SAFETY: precondition of function guarantees this is valid
    let ready = unsafe { std::slice::from_raw_parts(ready_ptr, config.num_warps) };
    let valid = unsafe { std::slice::from_raw_parts_mut(valid_ptr, config.num_warps) };
    let tmask = unsafe { std::slice::from_raw_parts_mut(tmask_ptr, config.num_warps) };
    let pc = unsafe { std::slice::from_raw_parts_mut(pc_ptr, config.num_warps) };
    let op = unsafe { std::slice::from_raw_parts_mut(opcode_ptr, config.num_warps) };
    let f3 = unsafe { std::slice::from_raw_parts_mut(f3_ptr, config.num_warps) };
    let rd_addr = unsafe { std::slice::from_raw_parts_mut(rd_addr_ptr, config.num_warps) };
    let rs1_addr = unsafe { std::slice::from_raw_parts_mut(rs1_addr_ptr, config.num_warps) };
    let rs2_addr = unsafe { std::slice::from_raw_parts_mut(rs2_addr_ptr, config.num_warps) };
    let rs3_addr = unsafe { std::slice::from_raw_parts_mut(rs3_addr_ptr, config.num_warps) };
    let f7 = unsafe { std::slice::from_raw_parts_mut(f7_ptr, config.num_warps) };
    let imm32 = unsafe { std::slice::from_raw_parts_mut(imm32_ptr, config.num_warps) };
    let imm24 = unsafe { std::slice::from_raw_parts_mut(imm24_ptr, config.num_warps) };
    let csr_imm = unsafe { std::slice::from_raw_parts_mut(csr_imm_ptr, config.num_warps) };
    let finished = unsafe { finished_ptr.as_mut().expect("pointer was null") };

    for (w, o_line) in warp_insts.iter().enumerate() {
        match o_line {
            Some(line) => {
                valid[w] = 1;
                tmask[w] = line.tmask;
                pc[w] = line.pc;
                op[w] = line.opcode as u32;
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
    for maybe_inst in warp_insts.iter() {
        match maybe_inst {
            Some(inst) => {
                println!("trace: {}", inst);
            }
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

    *finished = sim.finished() as u8;
}

mod backend_model;
mod mem_model;

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
    issue_csr_imm: u8,
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

    let to_vec = |slice: &[u32]| slice.iter().map(|u| Some(*u)).collect::<Vec<_>>();

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
        rs4_addr: 0, /* unused */
        rs1_data: to_vec(issue_rs1_data),
        rs2_data: to_vec(issue_rs2_data),
        rs3_data: to_vec(issue_rs3_data),
        rs4_data: to_vec(issue_rs3_data), /* unused */
        f7: issue_f7,
        imm32: issue_imm32,
        imm24: issue_imm24 as i32,
        csr_imm: issue_csr_imm,
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
