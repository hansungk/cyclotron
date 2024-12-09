use std::collections::HashMap;
use std::sync::Arc;
use log::{debug, info};
use crate::base::behavior::{Resets, Stalls, Ticks};
use crate::base::component::{ComponentBase, IsComponent};
use crate::base::mem::HasMemory;
use crate::base::state::HasState;
use crate::muon::decode::DecodedInst;
use crate::muon::isa::{InstAction, ISA};

pub struct Writeback {
    pub rd_addr: u8,
    pub rd_data: u32,
    pub set_pc: Option<u32>,
}

// a sparse memory structure that initializes
// anything read with 0
// TODO: this needs to work with imem
#[derive(Default)]
struct ToyMemory {
    mem: HashMap<usize, u32>,
}

impl HasMemory for ToyMemory {
    fn read<const N: usize>(&mut self, addr: usize) -> Option<Arc<[u8; N]>> {
        assert!((N % 4 == 0) && N > 0, "word sized requests only");
        let words: Vec<_> = (addr..addr + N).step_by(4).map(|a| {
            if !self.mem.contains_key(&a) {
                self.mem.insert(a, 0u32);
            }
            self.mem[&a]
        }).collect();

        let byte_array: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        Some(Arc::new(byte_array.try_into().unwrap()))
    }

    fn write<const N: usize>(&mut self, addr: usize, data: Arc<[u8; N]>) -> Result<(), String> {
        assert!((N % 4 == 0) && N > 0, "word sized requests only");
        (0..N).step_by(4).for_each(|a| {
            let write_slice = &data[a..a + 4];
            self.mem.insert(addr + a, u32::from_le_bytes(write_slice.try_into().unwrap()));
        });
        Ok(())
    }
}

#[derive(Default)]
struct ExecuteUnitState {
    dmem: ToyMemory,
}

#[derive(Default)]
pub struct ExecuteUnit {
    base: ComponentBase<ExecuteUnitState>,
}

impl Ticks for ExecuteUnit {
    fn tick_one(&mut self) {}
}

impl Resets for ExecuteUnit {
    fn reset(&mut self) {
        self.base.state.dmem.mem.clear();
    }
}

impl Stalls for ExecuteUnit {}
impl HasState for ExecuteUnit {}

impl IsComponent<ExecuteUnitState> for ExecuteUnit {
    fn get_base(&mut self) -> &mut ComponentBase<ExecuteUnitState> { &mut self.base }
}

impl ExecuteUnit {
    pub fn execute(&mut self, decoded: DecodedInst) -> Writeback {
        let isa = ISA::get_insts();
        info!("executing decoded instruction {}", decoded);
        let (alu_result, actions) = isa.iter().map(|inst_group| {
            inst_group.execute(&decoded)
        }).fold(None, |prev, curr| {
            assert!(prev.and(curr).is_none(), "multiple viable implementations for {}", &decoded);
            prev.or(curr)
        }).expect(&format!("unimplemented instruction {}", &decoded));

        let mut writeback = Writeback {
            rd_addr: 0,
            rd_data: 0,
            set_pc: None,
        };
        if (actions & InstAction::WRITE_REG) > 0 {
            writeback.rd_addr = decoded.rd;
            writeback.rd_data = alu_result;
        }
        if (actions & InstAction::MEM_LOAD) > 0 {
            let load_data_bytes = self.base.state.dmem.read::<4>(alu_result as usize);
            writeback.rd_addr = decoded.rd;
            writeback.rd_data = u32::from_le_bytes(*load_data_bytes.unwrap());
        }
        if (actions & InstAction::MEM_STORE) > 0 {
            self.base.state.dmem.write::<4>(alu_result as usize, Arc::new(decoded.rs2.to_le_bytes())).unwrap();
        }
        if (actions & InstAction::SET_REL_PC) > 0 {
            writeback.set_pc = (alu_result != 0).then(|| decoded.pc + alu_result);
        }
        if (actions & InstAction::SET_ABS_PC) > 0 {
            writeback.set_pc = Some(alu_result);
        }
        if (actions & InstAction::LINK) > 0 {
            writeback.rd_addr = decoded.rd;
            writeback.rd_data = decoded.pc + 8;
        }
        if (actions & InstAction::FENCE) > 0 {
            todo!();
        }

        writeback
    }
}
