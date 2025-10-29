//! This module allows Cyclotron to be used as a backend memory model within a Verilog testbench.
//! The memory interface it provides is kept separate from the memory that Cyclotron itself uses to produce the 
//! golden architectural trace (though both are initialized with the same contents).

use std::{ffi::c_void, iter::zip, slice};

use crate::{base::mem::HasMemory, sim::flat_mem::FlatMemory};

struct LsuMemRequest {
    store: bool,
    tag: u32,
    address: Vec<u32>,
    data: Vec<u32>,
    mask: Vec<bool>,
}

// Mirrors hardware bundle
struct LsuMemResponse {
    tag: u32,
    data: Vec<u32>,
    valid: Vec<bool>
}

// Interface for Cyclotron memory models
trait LsuMemModel {
    fn tick(&mut self);
    fn consume_request(&mut self, req: LsuMemRequest) -> bool;
    fn produce_response(&mut self) -> Option<LsuMemResponse>;
}

/// Super basic memory model: 1-cycle latency, never exerts backpressure, FIFO response order
pub(super) struct BasicMemModel {
    mem: FlatMemory,
    pending_reqs: Vec<LsuMemRequest>,
    pending_resp: Option<LsuMemResponse>,

    lsu_lanes: usize,
}

impl BasicMemModel {
    pub(super) fn new(lsu_lanes: usize) -> Self {
        Self {
            mem: FlatMemory::new(None),
            pending_reqs: Vec::new(),
            pending_resp: None,

            lsu_lanes
        }
    }
}

impl LsuMemModel for BasicMemModel {
    fn tick(&mut self) {
        if self.pending_resp.is_some() {
            return;
        }

        if self.pending_reqs.is_empty() {
            return;
        }

        let req = self.pending_reqs.remove(0);

        let resp = if req.store {
            for i in 0..self.lsu_lanes {
                if !req.mask[i] {
                    continue;
                }

                self.mem.write(req.address[i] as usize, &req.data[i].to_le_bytes())
                    .expect("failed to write to mem");
            }

            LsuMemResponse {
                tag: req.tag,
                data: Vec::new(),
                valid: req.mask,
            }
        }
        else {
            let data = req.address.iter().map(|&address| {
                let bytes = self.mem.read_n::<4>(address as usize)
                    .expect("failed to read from mem");
                u32::from_le_bytes(bytes)
            }).collect();

            LsuMemResponse {
                tag: req.tag,
                data,
                valid: req.mask,
            }
        };

        self.pending_resp = Some(resp);
    }

    fn consume_request(&mut self, req: LsuMemRequest) -> bool {
        // always ready to accept requests
        self.pending_reqs.push(req);
        true
    }

    fn produce_response(&mut self) -> Option<LsuMemResponse> {
        std::mem::take(&mut self.pending_resp)
    }
}

#[no_mangle]
pub extern "C" fn cyclotron_mem_init_rs(
    lsu_lanes: usize,
) -> *mut c_void {
    let mem_model = Box::new(BasicMemModel::new(lsu_lanes));
    let mem_model_ptr = Box::into_raw(mem_model);

    mem_model_ptr as *mut c_void
}

#[no_mangle]
// SAFETY: must be called at most once on a pointer returned from `cyclotron_mem_init_rs`
pub extern "C" fn cyclotron_mem_free_rs(
    mem_model_ptr: *mut c_void
) {
    let mem_model = unsafe {
        Box::from_raw(mem_model_ptr as *mut BasicMemModel)
    };

    drop(mem_model);
}

// SAFETY: no other global function with this name exists
#[no_mangle]
/// DPI interface for issuing memory requests to a simulated memory subsystem.
/// Sample the pins on negedge, then call this function
pub extern "C" fn cyclotron_mem_rs(
    mem_model_ptr: *mut c_void,

    req_ready: *mut u8,
    req_valid: u8,

    req_store: u8,
    req_tag: u32,
    req_address: *const u32,
    req_data: *const u32,
    req_mask: *const u8,

    resp_ready: u8,
    resp_valid: *mut u8,

    resp_tag: *mut u32,
    resp_data: *mut u32,
    resp_valids: *mut u8,
) {
    // SAFETY: mem_model_ptr must be a pointer returned by `cyclotron_mem_init_rs` which has
    // not yet been freed using `cyclotron_mem_free_rs`
    let mem_model = unsafe { &mut *(mem_model_ptr as *mut BasicMemModel) };
    let (req_ready, req_address, req_data, req_mask, resp_valid, resp_tag, resp_data, resp_valids) = unsafe {(
        &mut *req_ready, 
        slice::from_raw_parts(req_address, mem_model.lsu_lanes),
        slice::from_raw_parts(req_data, mem_model.lsu_lanes),
        slice::from_raw_parts(req_mask, mem_model.lsu_lanes),
        
        &mut *resp_valid,
        &mut *resp_tag,
        slice::from_raw_parts_mut(resp_data, mem_model.lsu_lanes),
        slice::from_raw_parts_mut(resp_valids, mem_model.lsu_lanes),
    )};

    mem_model.tick();

    if req_valid != 0 {
        // construct req bundle
        let mut mask = Vec::with_capacity(mem_model.lsu_lanes);
        for &mask_bit in req_mask {
            mask.push(mask_bit != 0);
        }

        let data = Vec::from(req_data);
        let address = Vec::from(req_address);

        let mem_request = LsuMemRequest {
            store: req_store != 0,
            address,
            tag: req_tag,
            data,
            mask,
        };

        let ready = mem_model.consume_request(mem_request);
        *req_ready = if ready { 1 } else { 0 };
    }
    else {
        *req_ready = 0;
    }

    if resp_ready == 0 {
        return;
    }

    if let Some(resp) = mem_model.produce_response() {
        *resp_tag = resp.tag;
        resp_data.copy_from_slice(&resp.data);
        for (hw_valid, resp_valid) in zip(resp_valids, resp.valid) {
            *hw_valid = if resp_valid { 1 } else { 0 };
        };

        *resp_valid = 1;
    }
    else {
        *resp_valid = 0;
    }
}