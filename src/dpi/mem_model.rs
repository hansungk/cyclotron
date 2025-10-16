//! This module allows Cyclotron to be used as a backend memory model within a Verilog testbench.
//! The memory interface it provides is kept separate from the memory that Cyclotron itself uses to produce the 
//! golden architectural trace (though both are initialized with the same contents).

use std::{ffi::c_void, iter::zip};

use crate::{base::mem::HasMemory, sim::flat_mem::FlatMemory};

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


/// Super basic memory model: 
pub(super) struct BasicMemModel {
    mem: FlatMemory,
    current_req: Option<MemRequest>,
    pending_resp: Option<MemResponse>,

    lsu_lanes: usize,
}

impl BasicMemModel {
    pub(super) fn new(lsu_lanes: usize) -> Self {
        Self {
            mem: FlatMemory::new(None),
            current_req: None,
            pending_resp: None,

            lsu_lanes
        }
    }
}

impl MemModel for BasicMemModel {
    fn tick(&mut self) {
        let Some(req) = std::mem::take(&mut self.current_req) 
            else { return };

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
        self.current_req = Some(req);
        true
    }

    fn produce_response(&mut self) -> Option<MemResponse> {
        std::mem::take(&mut self.pending_resp)
    }
}

pub extern "C" fn cyclotron_mem_init_rs(

) -> *mut c_void {
    // TODO: consolidate w/ cyclotron backend model?

    todo!()
}

// SAFETY: no other global function with this name exists
#[unsafe(no_mangle)]
/// DPI interface for issuing memory requests to a simulated memory subsystem.
/// Sample the pins on negedge, then call this function
pub extern "C" fn cyclotron_mem_rs(
    global_req_ready: *mut u8,
    global_req_valid: u8,

    global_req_store: u8,
    global_req_address: u32,
    // global_req_size: u32,
    global_req_tag: u32,
    global_req_data: *const u32,
    global_req_mask: *const u32,

    global_resp_ready: u8,
    global_resp_valid: *mut u8,

    global_resp_tag: u32,
    global_resp_data: *mut u32,
) {

}