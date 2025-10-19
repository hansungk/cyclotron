//! This module allows Cyclotron to be used as a backend memory model within a Verilog testbench.
//! The memory interface it provides is kept separate from the memory that Cyclotron itself uses to produce the 
//! golden architectural trace (though both are initialized with the same contents).

use std::{ffi::c_void, iter::zip, slice};

use crate::{base::mem::HasMemory, sim::flat_mem::FlatMemory, utils::BitSlice};

// Mirrors hardware bundle
struct MemRequest {
    store: bool,
    address: u32,
    tag: u32,
    data: Vec<u8>,
    mask: Vec<bool>,
}

// Mirrors hardware bundle
struct MemResponse {
    tag: u32,
    data: Vec<u8>
}

// Interface for Cyclotron memory models
trait MemModel {
    fn tick(&mut self);
    fn consume_request(&mut self, req: MemRequest) -> bool;
    fn produce_response(&mut self) -> Option<MemResponse>;
}


/// Super basic memory model: 1-cycle latency, never exerts backpressure, FIFO response order
pub(super) struct BasicMemModel {
    mem: FlatMemory,
    pending_reqs: Vec<MemRequest>,
    pending_resp: Option<MemResponse>,

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

impl MemModel for BasicMemModel {
    fn tick(&mut self) {
        if self.pending_resp.is_some() {
            return;
        }

        if self.pending_reqs.is_empty() {
            return;
        }

        let req = self.pending_reqs.remove(0);

        let resp = if req.store {
            let buf = self.mem.read(req.address as usize, req.data.len())
                .expect("failed to read from mem");
            let mut buf = Vec::from(buf);
            
            for (i, (mask_bit, byte)) in zip(req.mask, req.data).enumerate() {
                if mask_bit { buf[i] = byte }
            }

            self.mem.write(
                req.address as usize,
                &buf
            ).expect("failed to write to mem");

            MemResponse {
                tag: req.tag,
                data: Vec::new(),
            }
        }
        else {
            let data = Vec::from(
                self.mem.read(req.address as usize, self.lsu_lanes).expect("failed to read from mem")
            );

            MemResponse {
                tag: req.tag,
                data
            }
        };

        self.pending_resp = Some(resp);
    }

    fn consume_request(&mut self, req: MemRequest) -> bool {
        // always ready to accept requests
        self.pending_reqs.push(req);
        true
    }

    fn produce_response(&mut self) -> Option<MemResponse> {
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
    req_address: u32,
    // global_req_size: u32,
    req_tag: u32,
    req_data: *const u32,
    req_mask: *const u32,

    resp_ready: u8,
    resp_valid: *mut u8,

    resp_tag: *mut u32,
    resp_data: *mut u32,
) {
    // SAFETY: mem_model_ptr must be a pointer returned by `cyclotron_mem_init_rs` which has
    // not yet been freed using `cyclotron_mem_free_rs`
    let mem_model = unsafe { &mut *(mem_model_ptr as *mut BasicMemModel) };
    let (req_ready, req_data, req_mask, resp_valid, resp_tag, resp_data) = unsafe {(
        &mut *req_ready, 
        slice::from_raw_parts(req_data, mem_model.lsu_lanes),
        slice::from_raw_parts(req_mask, (mem_model.lsu_lanes + 31) / 32),
        
        &mut *resp_valid,
        &mut *resp_tag,
        slice::from_raw_parts_mut(resp_data, mem_model.lsu_lanes),
    )};

    mem_model.tick();

    if req_valid != 0 {
        // construct req bundle
        let mut mask = Vec::with_capacity(mem_model.lsu_lanes);
        for byte in 0..(req_data.len() * 4) {
            let (idx, bit) = (byte / 8, byte % 8);
            mask.push(req_mask[idx].bit(bit));
        }

        let data = req_data.iter()
            .map(|i| i.to_le_bytes())
            .flatten()
            .collect();

        let mem_request = MemRequest {
            store: req_store != 0,
            address: req_address,
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
        let (_, data, _) = unsafe { resp.data.as_slice().align_to() };
        resp_data.copy_from_slice(data);

        *resp_valid = 1;
    }
    else {
        *resp_valid = 0;
    }
}