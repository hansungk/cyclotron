// #![allow(dead_code, unreachable_code)]
use crate::base::behavior::*;
use crate::base::module::IsModule;
use crate::muon::core::MuonCore;
use crate::muon::decode::{DecodedInst, MicroOp};
use crate::sim::top::Sim;
use crate::sim::trace;
use crate::ui::CyclotronArgs;
use log::debug;
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
    issue_queue: Vec<IssueQueue>,
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
type IssueQueue = VecDeque<IssueQueueLine>;

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
        // DPI context is already initialized by some other call; exit
        return;
    }

    let log_level = LevelFilter::Debug;
    Builder::new().filter_level(log_level).init();

    // let toml_path = PathBuf::from("config.toml");
    // let toml_string = crate::ui::read_toml(&toml_path);

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
    let sim_isa = crate::ui::make_sim(None, &arg);
    let sim_be = crate::ui::make_sim(None, &arg);
    assert_single_core(&sim_isa);
    assert_single_core(&sim_be);

    let final_elfname = sim_isa.config.elf.as_path();
    // TODO: print config summary
    println!(
        "Cyclotron: created sim object with ELF file: {}",
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

#[no_mangle]
/// Get un-decoded instruction bits from the instruction trace.
pub unsafe extern "C" fn cyclotron_fetch_rs(
    req_valid: u8,
    req_bits_tag: u64,
    req_bits_pc: u32,
    resp_valid_ptr: *mut u8,
    resp_bits_tag_ptr: *mut u64,
    resp_bits_inst_ptr: *mut u64,
) {
    let mut context_guard = CELL.write().unwrap();
    let context = context_guard.as_mut().expect("DPI context not initialized!");
    let sim = &mut context.sim_isa;
    let core = &mut sim.top.clusters[0].cores[0];

    let resp_valid = unsafe { resp_valid_ptr.as_mut().expect("pointer was null") };
    let resp_bits_tag = unsafe { resp_bits_tag_ptr.as_mut().expect("pointer was null") };
    let resp_bits_inst = unsafe { resp_bits_inst_ptr.as_mut().expect("pointer was null") };

    *resp_valid = 0;
    *resp_bits_tag = 0;
    *resp_bits_inst = 0;
    if req_valid != 1 {
        return;
    }

    let inst = core.fetch(0 /*warp*/, req_bits_pc);

    // 1-cycle latency
    *resp_valid = 1;
    *resp_bits_tag = req_bits_tag;
    *resp_bits_inst = inst;
}

fn peek_heads(c: &MuonCore, num_warps: usize) -> Vec<Option<trace::Line>> {
    (0..num_warps)
        .map(|w| c.get_tracer().peek(w).cloned())
        .collect::<Vec<_>>()
}

fn push_issue_queue(c: &mut MuonCore, issue_queue: &mut Vec<IssueQueue>, per_warp_pop: &[bool]) {
    let num_warps = per_warp_pop.len();
    let heads = peek_heads(c, num_warps);
    for (w, (o_line, pop)) in zip(heads, per_warp_pop).enumerate() {
        if o_line.is_some() && *pop {
            c.get_tracer_mut().consume(w);
            let iline = IssueQueueLine {
                line: o_line.unwrap(),
                checked: false,
            };
            issue_queue[w].push_back(iline);
        }
    }
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

    // upon RTL fire, consume the instruction line in the tracer queue, and move it to the issue
    // queue.  This has to happen before the sim::tick() call below, so that it respects the
    // dequeue->enqueue data hazard
    let per_warp_ready = ready.iter().map(|r| *r == 1).collect::<Vec<bool>>();
    push_issue_queue(core, &mut context.issue_queue, &per_warp_ready);

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
    let writeback_ipdom_restored_mask =
        unsafe { writeback_ipdom_restored_mask_ptr.as_mut().expect("pointer was null") };
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

    core.warps[issue_warp_id as usize]
        .state_mut()
        .csr_file
        .iter_mut()
        .for_each(|c| {
            // c.emu_access(0xcc3, schedule.active_warps);
            c.emu_access(0xcc4, issue_tmask);
        });

    debug!("issue warp id is {}", issue_warp_id);
    debug!("{}", decoded);

    let writeback = core.warps[issue_warp_id as usize]
        .backend(
            MicroOp {
                inst: decoded,
                tmask: issue_tmask,
            },
            &mut core.scheduler,
            neutrino,
            &mut core.shared_mem,
        )
        .expect("uh");
    // let writeback = core.execute(issue_warp_id.into(), issued, issue_tmask, neutrino);
    // Warp::writeback(&writeback, core.warps[issue_warp_id as usize].base.state.reg_file.as_mut());
    // let rf2 = &core.warps[issue_warp_id as usize].base.state.reg_file;
    // debug!("{:#?}", rf2.iter().map(|r| Some(r.read_gpr(issue_rs1_addr))).collect::<Vec<_>>());

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
    *writeback_ipdom_restored_mask = writeback.sched_wb.ipdom_push.map(|x| x.restored_mask).unwrap_or(0);
    *writeback_ipdom_else_mask = writeback.sched_wb.ipdom_push.map(|x| x.else_mask).unwrap_or(0);
    *writeback_ipdom_else_pc = writeback.sched_wb.ipdom_push.map(|x| x.else_pc).unwrap_or(0);
    *finished = writeback.sched_wb.tohost.is_some() as u8;

    // for (data_pin, owb) in zip(writeback_rd_data, writeback.rd_data) {
    //     *data_pin = owb.unwrap_or(0);
    // }
}

#[no_mangle]
/// Do a differential test between the register values read at instruction issue from RTL, and the
/// values logged in Cyclotron trace.
pub unsafe extern "C" fn cyclotron_difftest_reg_rs(
    sim_tick: u8,
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
    let config = sim.top.clusters[0].cores[0].conf().clone();

    // runs regardless of valid == 0/1 to ensure model run-ahead of rtl
    if sim_tick == 1 {
        sim.tick();

        let all_warp_pop = vec![true; config.num_warps];
        let core = &mut sim.top.clusters[0].cores[0];
        push_issue_queue(core, &mut context.issue_queue, &all_warp_pop);
    }

    if valid == 0 {
        return;
    }
    let rs1_data = unsafe { std::slice::from_raw_parts(rs1_data_raw, config.num_lanes) };
    let rs2_data = unsafe { std::slice::from_raw_parts(rs2_data_raw, config.num_lanes) };
    let rs3_data = unsafe { std::slice::from_raw_parts(rs3_data_raw, config.num_lanes) };

    let isq = &mut context.issue_queue[warp_id as usize];

    if isq.is_empty() {
        println!(
            "DIFFTEST fail: rtl over-ran model, first remaining inst: pc:{:x}, warp:{}",
            pc, warp_id
        );
        panic!("DIFFTEST fail");
    }

    // iter_mut() order equals the enqueue order, which equals the program order.  This way we
    // match RTL against the oldest same-PC model instruction
    let mut checked = false;
    for line in isq.iter_mut() {
        if line.line.pc != pc {
            continue;
        }
        if line.checked {
            continue;
        }

        let compare_reg_addr_and_exit = |_rtl: u8, _model: u8, _name: &str| {
            // if rtl != model {
            //     println!(
            //         "DIFFTEST fail: {} address mismatch, pc:{:x}, warp: {}, rtl:{}, model:{}",
            //         name, pc, warp_id, rtl, model,
            //     );
            //     panic!("DIFFTEST fail");
            // }
        };
        let compare_reg_data_and_exit = |rtl: &[u32], model: &[Option<u32>], name: &str| {
            let res = compare_vector_reg_data(rtl, model);
            if let Err(e) = res {
                println!(
                    "DIFFTEST fail: {} data mismatch, pc:{:x}, warp:{}, lane:{}, rtl:{}, model:{}",
                    name, pc, warp_id, e.lane, e.rtl, e.model
                );
                panic!("DIFFTEST fail");
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
        checked = true;
        break;
    }

    if !checked {
        println!(
            "DIFFTEST fail: pc mismatch: rtl reached instruction unseen by model, pc:{:x}, warp:{}",
            pc, warp_id
        );
        panic!("DIFFTEST fail");
    }

    // TODO: check rtl under-run of model

    // clean up checked lines at the front
    clear_front_checked(isq);

    context.difftested_insts += 1;
    if context.difftested_insts % 100 == 0 {
        println!("DIFFTEST: Checked {} instructions", context.difftested_insts);
    }
}

fn clear_front_checked(isq: &mut IssueQueue) {
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

#[no_mangle]
/// Gather performance monitoring counters from Muon and generate high-level performance metrics
/// and pipeline bottleneck analysis.
pub unsafe extern "C" fn profile_perf_counters_rs(
    inst_retired: u64,
    cycles: u64,
    cycles_decoded: u64,
    cycles_eligible: u64,
    cycles_issued: u64,
    per_warp_cycles_decoded_ptr: *const u64,
    per_warp_stalls_waw_ptr: *const u64,
    per_warp_stalls_war_ptr: *const u64,
    finished: u8,
) {
    if finished != 1 {
        return;
    }

    let mut context_guard = CELL.write().unwrap();
    let context = context_guard.as_mut().expect("DPI context not initialized!");
    let sim = &mut context.sim_isa;
    let core = &mut sim.top.clusters[0].cores[0];
    let config = core.conf().clone();

    let per_warp_cycles_decoded = unsafe { std::slice::from_raw_parts(per_warp_cycles_decoded_ptr, config.num_warps) };
    let per_warp_stalls_waw = unsafe { std::slice::from_raw_parts(per_warp_stalls_waw_ptr, config.num_warps) };
    let per_warp_stalls_war = unsafe { std::slice::from_raw_parts(per_warp_stalls_war_ptr, config.num_warps) };
    let all_warp_cycles_decoded: u64 = per_warp_cycles_decoded.iter().sum();
    let all_warp_stalls_waw: u64 = per_warp_stalls_waw.iter().sum();
    let all_warp_stalls_war: u64 = per_warp_stalls_war.iter().sum();
    let avg_warp_stalls_waw = all_warp_stalls_waw as f32 / all_warp_cycles_decoded as f32;
    let avg_warp_stalls_war = all_warp_stalls_war as f32 / all_warp_cycles_decoded as f32;
    let avg_active_warps = all_warp_cycles_decoded as f32 / cycles_decoded as f32;

    let ipc = inst_retired as f32 / cycles as f32;
    let frac = |cycle: u64| { cycle as f32 / cycles as f32 };
    let percent = |cycle| { frac(cycle) * 100. };

    println!("Muon core finished execution.");
    println!("");
    println!("+-----------------------+");
    println!(" Muon Performance Report");
    println!("+-----------------------+");
    println!("Instructions: {}", inst_retired);
    println!("Cycles: {}", cycles);
    println!("├─ with decoded insts: {} ({:.2}%)", cycles_decoded, percent(cycles_decoded));
    println!("├─ with eligible insts: {} ({:.2}%)", cycles_eligible, percent(cycles_eligible));
    println!("└─ with issued insts: {} ({:.2}%)", cycles_issued, percent(cycles_issued));
    println!("Per-warp cycles:");
    println!("├─ with decoded insts [warp 0]: {}", per_warp_cycles_decoded[0]);
    println!("├─ avg. active warps: {}", avg_active_warps);
    println!("├─ avg. stalls due to write-after-write: {:.2}", avg_warp_stalls_waw);
    println!("└─ avg. stalls due to write-after-read:  {:.2}", avg_warp_stalls_war);
    println!("IPC: {:.3}", ipc);
    println!("+-----------------------+");
}

mod mem_model;
