use crate::muon::config::MuonConfig;
use crate::muon::warp::Writeback;
use std::collections::VecDeque;
use std::iter::zip;

/// Queues instruction traces from a core into a buffer, and provides a per-warp consume()
/// interface to dequeue instructions from the buffer in program-order.
pub struct Tracer {
    /// per-warp program-order buffer of lines
    bufs: Vec<VecDeque<Line>>,
}

#[derive(Default)]
pub struct Line {
    pub pc: u32,
    pub opcode: u16,
    pub rd_addr: u8,
    pub rs1_addr: u8,
    pub rs2_addr: u8,
    pub rs3_addr: u8,
    pub rs4_addr: u8,
    pub rd_data: Vec<Option<u32>>,
    pub rs1_data: Vec<Option<u32>>,
    pub rs2_data: Vec<Option<u32>>,
    pub rs3_data: Vec<Option<u32>>,
    pub rs4_data: Vec<Option<u32>>,
    pub f3: u8,
    pub f7: u8,
    pub imm32: u32,
    pub imm24: i32,
    pub imm8: i32,
    pub tmask: u32,
}

impl Tracer {
    pub fn new(muon_config: &MuonConfig) -> Tracer {
        let mut tracer = Tracer {
            bufs: Vec::new(),
        };
        (0..muon_config.num_warps).for_each(|_| {
            tracer.bufs.push(VecDeque::new());
        });
        tracer
    }

    pub fn record(&mut self, writebacks: &Vec<Option<Writeback>>) {
        zip(writebacks.iter(), self.bufs.as_mut_slice())
            .enumerate()
            .for_each(|(_wid, (wb, buf))| {
                if let Some(wb) = wb {
                    let line = Line {
                        pc: wb.inst.pc,
                        opcode: wb.inst.opcode,
                        rd_addr: wb.inst.rd,
                        rs1_addr: wb.inst.rs1_addr,
                        rs2_addr: wb.inst.rs2_addr,
                        rs3_addr: wb.inst.rs3_addr,
                        rs4_addr: wb.inst.rs4_addr,
                        rd_data: wb.rd_data.clone(),
                        f3: wb.inst.f3,
                        f7: wb.inst.f7,
                        imm32: wb.inst.imm32,
                        imm24: wb.inst.imm24,
                        imm8: wb.inst.imm8,
                        tmask: wb.tmask,
                        ..Line::default() // TODO: rs1/2/3/4_data
                    };
                    buf.push_back(line);
                    // println!("trace: pushed line to buf for warp={}; len={}", wid, buf.len());
                }
            })
    }

    pub fn consume(&mut self, warp_id: usize) -> Option<Line> {
        self.bufs[warp_id].pop_front()
    }
}
