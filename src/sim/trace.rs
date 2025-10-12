use crate::muon::config::MuonConfig;
use crate::muon::warp::Writeback;
use std::collections::VecDeque;
use std::fmt;
use std::iter::zip;

/// Queues instruction traces from a core into a buffer, and provides a per-warp consume()
/// interface to dequeue instructions from the buffer in program-order.
pub struct Tracer {
    /// per-warp program-order buffer of lines
    bufs: Vec<VecDeque<Line>>,
    /// round-robin warp selector for consumption
    rr: usize,
}

#[derive(Default, Clone, Debug)]
pub struct Line {
    pub warp_id: u32,
    pub pc: u32,
    pub opcode: u8,
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
    pub tmask: u32,
}

impl fmt::Display for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "TraceLine: (warp:{}, pc:0x{:x}, opcode:{}, rd:{})",
            self.warp_id, self.pc, self.opcode, self.rd_addr
        )
    }
}

impl Tracer {
    pub fn new(muon_config: &MuonConfig) -> Tracer {
        let mut tracer = Tracer {
            bufs: Vec::new(),
            rr: 0,
        };
        (0..muon_config.num_warps).for_each(|_| {
            tracer.bufs.push(VecDeque::new());
        });
        tracer
    }

    pub fn record(&mut self, writebacks: &Vec<Option<Writeback>>) {
        for (wid, (wb, buf)) in
            zip(writebacks.iter(), self.bufs.as_mut_slice()).enumerate() {
            if let Some(wb) = wb {
                let line = Line {
                    warp_id: wid as u32,
                    pc: wb.inst.pc,
                    opcode: wb.inst.opcode,
                    rd_addr: wb.inst.rd_addr,
                    rs1_addr: wb.inst.rs1_addr,
                    rs2_addr: wb.inst.rs2_addr,
                    rs3_addr: wb.inst.rs3_addr,
                    rs4_addr: wb.inst.rs4_addr,
                    rd_data: wb.rd_data.clone(),
                    f3: wb.inst.f3,
                    f7: wb.inst.f7,
                    imm32: wb.inst.imm32,
                    imm24: wb.inst.imm24,
                    tmask: wb.tmask,
                    ..Line::default() // TODO: rs1/2/3/4_data
                };
                buf.push_back(line);
                // println!("trace: pushed line to buf for warp={}; len={}", wid, buf.len());
            }
        }
    }

    pub fn peek(&mut self, warp_id: usize) -> Option<&Line> {
        self.bufs[warp_id].front()
    }

    pub fn consume(&mut self, warp_id: usize) -> Option<Line> {
        self.bufs[warp_id].pop_front()
    }

    /// Consume an instruction from any warp's buffer, round-robinning the warp selection.
    pub fn consume_round_robin(&mut self) -> Option<Line> {
        for i in 0..self.bufs.len() {
            let wid = self.rr + i;
            let buf = &self.bufs[wid];
            if buf.len() == 0 {
                continue;
            }
            return self.consume(wid);
        }
        None
    }

    pub fn len(&self, warp_id: usize) -> usize {
        self.bufs[warp_id].len()
    }

    pub fn all_empty(&self) -> bool {
        self.bufs.iter().all(|buf| buf.len() == 0)
    }
}
