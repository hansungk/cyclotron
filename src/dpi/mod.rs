#![allow(dead_code, unused_variables, unreachable_code)]
use crate::base::behavior::*;
use crate::sim::top::Sim;
use std::iter::zip;
use std::path::PathBuf;
use std::sync::RwLock;

struct Context {
    sim: Sim,
}

/// Global singleton to maintain simulator context across independent DPI calls.
static CELL: RwLock<Option<Context>> = RwLock::new(None);

struct ReqBundle {
    valid: bool,
    size: u32,
    address: u64,
}

struct RespBundle {
    valid: bool,
    _size: u32,
    data: [u8; 8],
}

#[no_mangle]
/// Entry point to the DPI interface.  This must be called from Verilog once at the start in an
/// initial block.
pub fn cyclotron_init_rs() {
    let toml_path = PathBuf::from("config.toml");
    let toml_string = crate::ui::read_toml(&toml_path);
    let sim = crate::ui::make_sim(&toml_string, None);
    assert!(sim.top.clusters.len() == 1, "currently assumes model has 1 cluster and 1 core");
    assert!(sim.top.clusters[0].cores.len() == 1, "currently assumes model has 1 cluster and 1 core");

    println!("cyclotron_init_rs: created sim object from {}", toml_path.display());

    let mut c = Context {
        sim,
    };
    c.sim.top.reset();

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
    let sim = &mut context.sim;
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
    let sim = &mut context.sim;

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