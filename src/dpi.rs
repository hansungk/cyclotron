#![allow(dead_code, unused_variables, unreachable_code)]
use crate::base::behavior::*;
use crate::base::mem;
use crate::base::port::*;
use crate::sim::top::Sim;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::{OnceLock, RwLock};

struct Config {
    num_lanes: usize,
}

struct Context {
    sim: Sim,
    _mem_req: Port<InputPort, mem::MemRequest>,
    _mem_resp: Port<OutputPort, mem::MemResponse>,
}

static CONFIG_CELL: OnceLock<Config> = OnceLock::new();
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
pub fn cyclotron_init_rs(num_lanes: i32) {
    CONFIG_CELL.get_or_init(|| Config {
        num_lanes: num_lanes as usize,
    });

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
    raw_a_ready: *const u8,
    raw_a_valid: *mut u8,
    raw_a_address: *mut u64,
    _raw_a_is_store: *mut u8,
    raw_a_size: *mut u32,
    _raw_a_data: *mut u64,

    raw_d_ready: *mut u8,
    raw_d_valid: *const u8,
    _raw_d_is_store: *const u8,
    raw_d_size: *const u32,
    raw_d_data: *const u64,

    _inflight: u8,
    raw_finished: *mut u8,
) {
    let conf = CONFIG_CELL.get().unwrap();

    let mut context_guard = CELL.write().unwrap();
    let context = context_guard.as_mut().expect("DPI context not initialized!");
    let sim = &mut context.sim;
    assert!(sim.top.clusters.len() == 1, "currently assumes model has 1 cluster and 1 core");
    assert!(sim.top.clusters[0].cores.len() == 1, "currently assumes model has 1 cluster and 1 core");
    let core = &mut sim.top.clusters[0].cores[0];

    // TODO: match interface with DUT
    // TODO: get warp from DUT, consume that warp's buffer from model, and do diff-test

    let inst = core.get_tracer().consume(0);

    sim.tick();

    let slice_a_ready = unsafe { std::slice::from_raw_parts(raw_a_ready, conf.num_lanes) };
    let slice_a_valid = unsafe { std::slice::from_raw_parts_mut(raw_a_valid, conf.num_lanes) };
    let slice_a_address = unsafe { std::slice::from_raw_parts_mut(raw_a_address, conf.num_lanes) };
    let slice_a_size = unsafe { std::slice::from_raw_parts_mut(raw_a_size, conf.num_lanes) };
    let slice_d_ready = unsafe { std::slice::from_raw_parts_mut(raw_d_ready, conf.num_lanes) };
    let _finished = unsafe { std::slice::from_raw_parts_mut(raw_finished, 1) };

    // FIXME: only warp 0 is fed
    let req_bundles = Vec::with_capacity(1);
    todo!("no more imem ports");

    req_bundles_to_rtl(
        &req_bundles,
        slice_a_valid,
        slice_a_address,
        slice_a_size,
        slice_d_ready,
    );

    let slice_d_ready = unsafe { std::slice::from_raw_parts_mut(raw_d_ready, conf.num_lanes) };
    let slice_d_valid = unsafe { std::slice::from_raw_parts(raw_d_valid, conf.num_lanes) };
    let slice_d_size = unsafe { std::slice::from_raw_parts(raw_d_size, conf.num_lanes) };
    let slice_d_data = unsafe { std::slice::from_raw_parts(raw_d_data, conf.num_lanes) };

    // FIXME: work with 1 lane for now
    let mut resp_bundles = Vec::with_capacity(1);
    for i in 0..1 {
        slice_d_ready[i] = 1; // bogus?
        resp_bundles.push(RespBundle {
            valid: (slice_d_valid[i] != 0),
            _size: slice_d_size[i],
            data: slice_d_data[i].to_le_bytes(),
        });
    }

    todo!("no more imem ports");
    // let fsm = &mut context.fsm;
    // push_mem_resp(fsm, &resp_bundles[0]);
    // fsm.tick_one();

    // println!("[@{}] cyclotron_tick_rs()", muon.base.cycle);

    // // push_mem_resp(&mut muon.imem_resp[0], &resp_bundles[0]);
    // muon.tick_one();
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

#[cfg(test)]
mod tests {
    // use super::*;

    #[test]
    fn it_works() {
        // import_me()
    }
}
