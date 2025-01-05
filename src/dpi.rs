use crate::base::behavior::*;
use crate::base::component::IsComponent;
use crate::base::mem;
use crate::base::port::*;
// use crate::fsm::{Fsm, FsmConfig};
use crate::muon::config::MuonConfig;
use crate::muon::core::MuonCore;
use std::sync::Arc;
use std::sync::{OnceLock, RwLock};

struct Config {
    num_lanes: usize,
}

struct DpiContext {
    muon: MuonCore,
    mem_req: Port<InputPort, mem::MemRequest>,
    mem_resp: Port<OutputPort, mem::MemResponse>,
}

static CONFIG_CELL: OnceLock<Config> = OnceLock::new();
// A single-writer, multiple-reader mutex lock on the global singleton of Sim.
// A singleton is necessary because it's the only way to maintain context across independent DPI
// calls.  Cannot use OnceLock as it does not allow borrowing as mutable.
static CELL: RwLock<Option<DpiContext>> = RwLock::new(None);

struct ReqBundle {
    valid: bool,
    size: u32,
    address: u64,
}

struct RespBundle {
    valid: bool,
    size: u32,
    data: [u8; 8],
}

#[no_mangle]
pub fn emulator_init_rs(num_lanes: i32) {
    CONFIG_CELL.get_or_init(|| Config {
        num_lanes: num_lanes as usize,
    });

    println!(
        "emulator_init_rs(): configured with num_lanes={}",
        CONFIG_CELL.get().unwrap().num_lanes
    );

    let mut context = CELL.write().unwrap();
    if context.as_ref().is_some() {
        panic!("DPI context already initialized!");
    }
    let mut c = DpiContext {
        // TODO: propagate num_lanes to MuonConfig
        muon: MuonCore::new(Arc::new(MuonConfig::default())),
        mem_req: Port::new(),
        mem_resp: Port::new(),
    };

    // let fsm = &mut c.fsm;
    // link(&mut fsm.mem_req, &mut c.mem_req);
    // link(&mut fsm.mem_resp, &mut c.mem_resp);

    let muon = &mut c.muon;
    // FIXME: link should be here against DPI ports, but muon ports are already linked with its
    // internal modules
    muon.reset();

    *context = Some(c);
}

#[no_mangle]
pub fn emulator_tick_rs(
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
    let context = match context_guard.as_mut() {
        Some(s) => s,
        None => {
            panic!("DPI context not initialized!");
        }
    };
    // let fsm = &mut context.fsm;
    let muon = &mut context.muon;

    let slice_a_ready = unsafe { std::slice::from_raw_parts(raw_a_ready, conf.num_lanes) };
    let slice_a_valid = unsafe { std::slice::from_raw_parts_mut(raw_a_valid, conf.num_lanes) };
    let slice_a_address = unsafe { std::slice::from_raw_parts_mut(raw_a_address, conf.num_lanes) };
    let slice_a_size = unsafe { std::slice::from_raw_parts_mut(raw_a_size, conf.num_lanes) };
    let slice_d_ready = unsafe { std::slice::from_raw_parts_mut(raw_d_ready, conf.num_lanes) };
    let _finished = unsafe { std::slice::from_raw_parts_mut(raw_finished, 1) };

    // FIXME: only warp 0 is fed
    let mut req_bundles = Vec::with_capacity(1);
    match get_mem_req(&mut muon.imem_req[0], slice_a_ready[0] == 1) {
        Some(bundle) => {
            req_bundles.push(bundle);
        }
        None => {}
    }

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
            size: slice_d_size[i],
            data: slice_d_data[i].to_le_bytes(),
        });
    }

    // let fsm = &mut context.fsm;
    // push_mem_resp(fsm, &resp_bundles[0]);
    // fsm.tick_one();

    println!("[@{}] emulator_tick_rs()", context.muon.base.cycle);

    let muon = &mut context.muon;
    push_mem_resp(&mut muon.imem_resp[0], &resp_bundles[0]);
    muon.tick_one();
}

fn push_mem_resp(resp_port: &mut Port<InputPort, mem::MemResponse>, resp: &RespBundle) {
    if !resp.valid {
        return;
    }

    println!("RTL mem response pushed");
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
            size: 2 as u32, // FIXME: RTL doesn't support 256B yet
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
