#![allow(dead_code, unused_variables, unreachable_code)]
use crate::base::behavior::*;
use crate::base::mem;
use crate::base::port::*;
use crate::sim::top::Sim;
use std::iter::zip;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;

struct Context {
    sim: Sim,
    _mem_req: Port<InputPort, mem::MemRequest>,
    _mem_resp: Port<OutputPort, mem::MemResponse>,
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
pub fn cyclotron_init_rs() {
    let toml_path = PathBuf::from("config.toml");
    let toml_string = crate::ui::read_toml(&toml_path);
    let sim = crate::ui::make_sim(&toml_string, None);

    println!("cyclotron_init_rs: created sim object from {}", toml_path.display());

    let mut c = Context {
        sim,
        _mem_req: Port::new(),
        _mem_resp: Port::new(),
    };
    c.sim.top.reset();

    let mut context = CELL.write().unwrap();
    if context.as_ref().is_some() {
        panic!("DPI context already initialized!");
    }
    *context = Some(c);
}

#[no_mangle]
pub fn cyclotron_tick_rs(
    ibuf_ready_ptr: *const u8,
    ibuf_valid_ptr: *mut u8,
    ibuf_pc_ptr: *mut u32,
    ibuf_op_ptr: *mut u32,
    ibuf_rd_ptr: *mut u32,
    finished_ptr: *mut u8,
) {
    let mut context_guard = CELL.write().unwrap();
    let context = context_guard.as_mut().expect("DPI context not initialized!");
    let sim = &mut context.sim;
    assert!(sim.top.clusters.len() == 1, "currently assumes model has 1 cluster and 1 core");
    assert!(sim.top.clusters[0].cores.len() == 1, "currently assumes model has 1 cluster and 1 core");

    if !sim.finished() {
        sim.tick();
    }
    println!("cyclotron_tick_rs: sim tick!");

    let core = &mut sim.top.clusters[0].cores[0];
    let config = *core.conf();
    let warp_insts: Vec<_> = (0..config.num_warps).map(|w| {
        core.get_tracer().peek(w).cloned() // @perf: expensive?
    }).collect();

    let ibuf_ready = unsafe { std::slice::from_raw_parts(ibuf_ready_ptr, config.num_warps) };
    let ibuf_valid = unsafe { std::slice::from_raw_parts_mut(ibuf_valid_ptr, config.num_warps) };
    let ibuf_pc = unsafe { std::slice::from_raw_parts_mut(ibuf_pc_ptr, config.num_warps) };
    let ibuf_op = unsafe { std::slice::from_raw_parts_mut(ibuf_op_ptr, config.num_warps) };
    let ibuf_rd = unsafe { std::slice::from_raw_parts_mut(ibuf_rd_ptr, config.num_warps) };
    let finished = unsafe { std::slice::from_raw_parts_mut(finished_ptr, 1) };

    for (w, maybe_inst) in warp_insts.iter().enumerate() {
        match maybe_inst {
            Some(inst) => {
                ibuf_valid[w] = 1;
                ibuf_pc[w] = inst.pc;
                ibuf_op[w] = inst.opcode as u32;
                ibuf_rd[w] = inst.rd_addr as u32;
            },
            None => {
                ibuf_valid[w] = 0;
            }
        }
    }

    // consume if RTL ready was true
    // this has to happen after the pin drive above so that the consumed line is not lost
    zip(warp_insts, ibuf_ready).enumerate().for_each(|(w, (line, rdy))| {
        if line.is_some() && *rdy == 1 {
            println!("cyclotron_tick_rs: consumed warp {}", w);
            core.get_tracer().consume(w);
        }
    });

    let config = *core.conf();
    let warp_insts: Vec<_> = (0..config.num_warps).map(|w| {
        core.get_tracer().peek(w).cloned() // @perf: expensive?
    }).collect();

    finished[0] = sim.finished() as u8;

    // debug
    for maybe_inst in warp_insts.iter() {
        match maybe_inst {
            Some(inst) => { println!("trace: {}", inst); },
            _ => (),
        }
    }
}

// unwrap arrays-of-structs to structs-of-arrays
fn req_bundles_to_rtl(
    bundles: &[ReqBundle],
    slice_a_valid: &mut [u8],
    slice_a_address: &mut [u64],
    slice_a_size: &mut [u32],
    slice_d_ready: &mut [u8],
) {
    for i in 0..bundles.len() {
        slice_a_valid[i] = if bundles[i].valid { 1 } else { 0 };
        slice_a_address[i] = bundles[i].address;
        slice_a_size[i] = bundles[i].size;
        slice_d_ready[i] = 1; // FIXME: bogus
    }
}

fn push_mem_resp(resp_port: &mut Port<InputPort, mem::MemResponse>, resp: &RespBundle) {
    if !resp.valid {
        return;
    }

    println!("RTL mem response pushed");

    // FIXME: rather than pushing to Muon's input port, this should be pushing to DPI's own output
    // port.
    resp_port.put(&mem::MemResponse {
        op: mem::MemRespOp::Ack,
        data: Some(Arc::new(resp.data)),
    });
}

fn get_mem_req(
    req_port: &mut Port<OutputPort, mem::MemRequest>,
    mem_req_ready: bool,
) -> Option<ReqBundle> {
    if !mem_req_ready {
        return None;
    }

    let front = req_port.get();
    let req = front.map(|data| {
        println!(
            "imem req detected: address=0x{:x}, size={}",
            data.address, data.size
        );
        ReqBundle {
            valid: true,
            address: data.address as u64,
            size: 2u32, // FIXME: RTL doesn't support 256B yet
        }
    });
    req
}

#[cfg(test)]
mod tests {
    // use super::*;

    #[test]
    fn it_works() {
        // import_me()
    }
}
