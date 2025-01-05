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
    // fsm: Fsm,
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
    ready: bool,
    valid: bool,
    address: u64,
    size: u32,
}

struct RespBundle {
    valid: bool,
    size: u32,
}

#[no_mangle]
pub fn emulator_init_rs(num_lanes: i32) {
    CONFIG_CELL.get_or_init(|| Config {
        num_lanes: num_lanes as usize,
    });

    let mut context = CELL.write().unwrap();
    if context.as_ref().is_some() {
        panic!("DPI context already initialized!");
    }
    // *sim = Some(Fsm::new(Arc::new(FsmConfig::default())));

    let mut c = DpiContext {
        // fsm: Fsm::new(Arc::new(FsmConfig::default())),
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
    raw_d_ready: *mut u8,
    raw_d_valid: *const u8,
    _raw_d_is_store: *const u8,
    raw_d_size: *const u32,
) {
    let conf = CONFIG_CELL.get().unwrap();

    let vec_d_ready = unsafe { std::slice::from_raw_parts_mut(raw_d_ready, conf.num_lanes) };
    let vec_d_valid = unsafe { std::slice::from_raw_parts(raw_d_valid, conf.num_lanes) };
    let vec_d_size = unsafe { std::slice::from_raw_parts(raw_d_size, conf.num_lanes) };

    // FIXME: work with 1 lane for now
    let mut resp_bundles = Vec::with_capacity(1);
    for i in 0..1 {
        resp_bundles.push(RespBundle {
            valid: (vec_d_valid[i] != 0),
            size: vec_d_size[i],
        });
    }

    let mut context_guard = CELL.write().unwrap();
    let context = match context_guard.as_mut() {
        Some(c) => c,
        None => {
            panic!("sim cell not initialized!");
        }
    };

    // let fsm = &mut context.fsm;
    // push_mem_resp(fsm, &resp_bundles[0]);
    // fsm.tick_one();

    let muon = &mut context.muon;
    muon.tick_one();

    println!("emulator_tick_rs()");

    // for (ireq, iresp) in &mut muon.imem_req.iter_mut().zip(&mut muon.imem_resp) {
    //     if let Some(req) = ireq.get() {
    //         println!("imem req detected!");
    //         assert_eq!(req.size, 8, "imem read request is not 8 bytes");
    //         let succ = iresp.put(&mem::MemResponse {
    //             op: mem::MemRespOp::Ack,
    //             data: Some(Arc::new([0, 0, 0, 0, 0, 0, 0, 0])),
    //         });
    //         assert!(succ, "muon asserted fetch pressure, not implemented");
    //     }
    // }

    push_mem_resp(&mut muon.imem_resp[0], &resp_bundles[0]);

    muon.tick_one();
}

#[no_mangle]
pub fn emulator_generate_rs(
    raw_a_ready: *const u8,
    raw_a_valid: *mut u8,
    raw_a_address: *mut u64,
    _raw_a_is_store: *mut u8,
    raw_a_size: *mut u32,
    _raw_a_data: *mut u64,
    raw_d_ready: *mut u8,
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

    let vec_a_ready = unsafe { std::slice::from_raw_parts(raw_a_ready, conf.num_lanes) };
    let vec_a_valid = unsafe { std::slice::from_raw_parts_mut(raw_a_valid, conf.num_lanes) };
    let vec_a_address = unsafe { std::slice::from_raw_parts_mut(raw_a_address, conf.num_lanes) };
    let vec_a_size = unsafe { std::slice::from_raw_parts_mut(raw_a_size, conf.num_lanes) };
    let vec_d_ready = unsafe { std::slice::from_raw_parts_mut(raw_d_ready, conf.num_lanes) };
    let _finished = unsafe { std::slice::from_raw_parts_mut(raw_finished, 1) };

    let mut req_bundles = Vec::with_capacity(1);
    match get_mem_req(&mut muon.imem_req[0], vec_a_ready[0] == 1) {
        Some(bundle) => {
            req_bundles.push(bundle);
        }
        None => {}
    }

    req_bundles_to_vecs(
        &req_bundles,
        vec_a_valid,
        vec_a_address,
        vec_a_size,
        vec_d_ready,
    );
}

fn push_mem_resp(resp_port: &mut Port<InputPort, mem::MemResponse>, resp: &RespBundle) {
    if !resp.valid {
        return;
    }

    println!("RTL mem response pushed");
    resp_port.put(&mem::MemResponse {
        op: mem::MemRespOp::Ack,
        data: Some(Arc::new([0, 0, 0, 0, 0, 0, 0, 0])),
    });
}

fn get_mem_req(req_port: &mut Port<OutputPort, mem::MemRequest>, ready: bool) -> Option<ReqBundle> {
    let front = req_port.get();
    let req = front.map(|data| {
        println!("imem req detected");
        ReqBundle {
            valid: true,
            address: data.address as u64,
            size: data.size as u32,
            ready: true,
        }
    });
    assert!(ready, "only ready supported");
    req
}

// unwrap arrays-of-structs to structs-of-arrays
fn req_bundles_to_vecs(
    bundles: &[ReqBundle],
    vec_a_valid: &mut [u8],
    vec_a_address: &mut [u64],
    vec_a_size: &mut [u32],
    vec_d_ready: &mut [u8],
) {
    for i in 0..bundles.len() {
        vec_a_valid[i] = if bundles[i].valid { 1 } else { 0 };
        vec_a_address[i] = bundles[i].address;
        vec_a_size[i] = bundles[i].size;
        vec_d_ready[i] = 1; // FIXME: bogus
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
