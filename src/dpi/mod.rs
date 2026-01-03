// #![allow(dead_code, unreachable_code)]
use crate::base::behavior::*;
use crate::muon::core::MuonCore;
use crate::sim::top::Sim;
use crate::sim::trace;
use crate::ui::CyclotronArgs;
use std::collections::VecDeque;
use std::ffi::CStr;
use std::iter::zip;
use std::os::raw::c_char;
use std::path::PathBuf;
use std::sync::RwLock;

use env_logger::Builder;
use log::LevelFilter;

struct Context {
    sim_isa: Sim, // cyclotron instance for the ISA model
    // holds fetch/decoded, but not issued, instructions
    // to be used for diff-testing against RTL issue
    issue_queue: Vec<VecDeque<IssueQueueLine>>,
    sim_be: Sim, // cyclotron instance for the backend model
    cycles_after_finished: usize,
    difftested_insts: usize,
}

// must be large enough to let the core pipeline entirely drain
const FINISH_COUNTDOWN: usize = 100;

#[derive(Clone)]
struct IssueQueueLine {
    line: trace::Line,
    checked: bool,
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
/// Entry point to the DPI interface.  This must be called from Verilog at the start in an initial
/// block.  This function can be called multiple times in different .v shims.
pub extern "C" fn cyclotron_init_rs(c_elfname: *const c_char) {
    if CELL.read().unwrap().is_some() {
        // cyclotron is already initialized by some other call; exit
        return;
    }

    let log_level = LevelFilter::Debug;
    Builder::new().filter_level(log_level).init();

    let toml_path = PathBuf::from("config.toml");
    let toml_string = crate::ui::read_toml(&toml_path);

    let elfname = unsafe {
        if c_elfname.is_null() {
            String::new()
        } else {
            CStr::from_ptr(c_elfname).to_string_lossy().into_owned()
        }
    };
    let mut cyclotron_args = CyclotronArgs::default();
    if !elfname.is_empty() {
        cyclotron_args.binary_path = Some(PathBuf::from(&elfname));
    }

    // make separate sim instances for the golden ISA model and the backend model to prevent
    // double-execution on the same GMEM
    let arg = Some(cyclotron_args);
    let sim_isa = crate::ui::make_sim(&toml_string, &arg);
    let sim_be = crate::ui::make_sim(&toml_string, &arg);
    assert_single_core(&sim_isa);
    assert_single_core(&sim_be);

    let final_elfname = sim_isa.config.elf.as_path();
    println!(
        "Cyclotron: created sim object from {} [ELF file: {}]",
        toml_path.display(),
        final_elfname.display()
    );

    let mut c = Context {
        sim_isa,
        issue_queue: Vec::new(),
        sim_be,
        cycles_after_finished: 0,
        difftested_insts: 0,
    };
    c.sim_isa.top.reset();
    c.sim_be.top.reset();

    let config = &c.sim_isa.top.clusters[0].cores[0].conf().clone();
    c.issue_queue = vec![VecDeque::new(); config.num_warps];

    let mut context = CELL.write().unwrap();
    if context.as_ref().is_some() {
        panic!("DPI context already initialized!");
    }
    *context = Some(c);
}

#[no_mangle]
/// Get un-decoded instruction bits from the instruction trace.
pub unsafe extern "C" fn cyclotron_fetch_rs(fetch_pc: u32, fetch_warp: u32, inst_ptr: *mut u64) {
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
pub unsafe extern "C" fn cyclotron_frontend_rs(
    ibuf_ready_vec: *const u8,
    ibuf_valid_vec: *mut u8,
    ibuf_pc_vec: *mut u32,
    ibuf_wid_vec: *mut u8,
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
    ibuf_raw_vec: *mut u64,
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
    let wid = unsafe { std::slice::from_raw_parts_mut(ibuf_wid_vec, config.num_warps) };
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
    let raw = unsafe { std::slice::from_raw_parts_mut(ibuf_raw_vec, config.num_warps) };
    let finished = unsafe { finished_ptr.as_mut().expect("pointer was null") };

    // upon RTL fire, consume tracer queue and move line to the issue queue
    // this has to happen before the sim::tick() call below, so that it respects the
    // dequeue->enqueue data hazard
    // TODO: disable issue queue if no difftest
    let heads = peek_heads(core, config.num_warps);
    for (w, (o_line, rdy)) in zip(heads, ready).enumerate() {
        if o_line.is_some() && *rdy == 1 {
            core.get_tracer_mut().consume(w);
            let iline = IssueQueueLine {
                line: o_line.unwrap(),
                checked: false,
            };
            context.issue_queue[w].push_back(iline);
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
                wid[w] = line.warp_id as u8;
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
                raw[w] = line.raw;
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

    if sim.finished() {
        if context.cycles_after_finished == 0 {
            println!("Cyclotron: model finished execution");
        }
        context.cycles_after_finished += 1;
    }
    if context.cycles_after_finished == FINISH_COUNTDOWN && context.difftested_insts > 0 {
        println!("DIFFTEST: PASS: {} instructions", context.difftested_insts);
    }

    *finished = (context.cycles_after_finished >= FINISH_COUNTDOWN) as u8;
}

#[no_mangle]
/// Do a differential test between the register values read at instruction issue from RTL, and the
/// values logged in Cyclotron trace.
pub unsafe extern "C" fn cyclotron_difftest_reg_rs(
    valid: u8,
    // TODO: tmask
    pc: u32,
    warp_id: u32,
    rs1_enable: u8,
    rs1_address: u8,
    rs1_data_raw: *const u32,
    rs2_enable: u8,
    rs2_address: u8,
    rs2_data_raw: *const u32,
    rs3_enable: u8,
    rs3_address: u8,
    rs3_data_raw: *const u32,
) {
    let mut context_guard = CELL.write().unwrap();
    let context = context_guard.as_mut().expect("DPI context not initialized!");
    let sim = &mut context.sim_isa;

    let core = &mut sim.top.clusters[0].cores[0];
    let config = core.conf().clone();

    let rs1_data = unsafe { std::slice::from_raw_parts(rs1_data_raw, config.num_lanes) };
    let rs2_data = unsafe { std::slice::from_raw_parts(rs2_data_raw, config.num_lanes) };
    let rs3_data = unsafe { std::slice::from_raw_parts(rs3_data_raw, config.num_lanes) };

    if valid == 0 {
        return;
    }

    let isq = &mut context.issue_queue[warp_id as usize];
    if isq.is_empty() {
        println!(
            "DIFFTEST: rtl over-ran model, remnant rtl inst: pc:{:x}, warp:{}", pc, warp_id
        );
    }

    // iter_mut() order equals the enqueue order, which equals the program order.  This way we
    // match RTL against the oldest same-PC model instruction
    for line in isq.iter_mut() {
        if line.line.pc != pc {
            continue;
        }

        let compare_reg_addr_and_exit = |rtl: u8, model: u8, name: &str| {
            if rtl != model {
                println!(
                    "DIFFTEST: {} address match fail, pc:{:x}, warp: {}, rtl:{}, model:{}",
                    name, pc, warp_id, rtl, model,
                );
                panic!();
            }
        };
        let compare_reg_data_and_exit = |rtl: &[u32], model: &[Option<u32>], name: &str| {
            let res = compare_vector_reg_data(rtl, model);
            if let Err(e) = res {
                println!(
                    "DIFFTEST: {} data match fail, pc:{:x}, warp:{}, lane:{}, rtl:{}, model:{}",
                    name, pc, warp_id, e.lane, e.rtl, e.model
                );
                panic!();
            }
        };

        if rs1_enable != 0 {
            compare_reg_addr_and_exit(rs1_address, line.line.rs1_addr, "rs1");
            // sometimes the collector RTL drives bogus values on x0; ignore that.
            if rs1_address != 0 {
                compare_reg_data_and_exit(rs1_data, &line.line.rs1_data, "rs1");
            }
        }
        if rs2_enable != 0 {
            compare_reg_addr_and_exit(rs2_address, line.line.rs2_addr, "rs2");
            if rs2_address != 0 {
                compare_reg_data_and_exit(rs2_data, &line.line.rs2_data, "rs2");
            }
        }
        if rs3_enable != 0 {
            compare_reg_addr_and_exit(rs3_address, line.line.rs3_addr, "rs3");
            if rs3_address != 0 {
                compare_reg_data_and_exit(rs3_data, &line.line.rs3_data, "rs3");
            }
        }

        line.checked = true;
        break;
    }

    // TODO: check rtl under-run of model

    // clean up checked lines at the front
    let mut num_to_pop = 0;
    for line in isq.iter() {
        if line.checked == true {
            num_to_pop += 1;
        } else {
            break;
        }
    }
    for _ in 0..num_to_pop {
        isq.pop_front();
    }

    context.difftested_insts += 1;
    if context.difftested_insts % 100 == 0 {
        println!("DIFFTEST: Passed {} instructions", context.difftested_insts);
    }
}

struct RegMatchError {
    lane: usize,
    rtl: u32,
    model: u32,
}

fn compare_vector_reg_data(regs_rtl: &[u32], regs_model: &[Option<u32>]) -> Result<(), RegMatchError> {
    for (i, (rtl, model)) in zip(regs_rtl, regs_model).enumerate() {
        // TODO: instead of skipping None, compare with tmask
        if let Some(m) = model {
            if *rtl != *m {
                return Err(RegMatchError {
                    lane: i,
                    rtl: *rtl,
                    model: *m,
                });
            }
        }
    }

    Ok(())
}

mod backend_model;
mod mem_model;
