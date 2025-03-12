use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use log::info;
use crate::base::mem::HasMemory;
use crate::sim::elf::ElfBackedMem;

// a sparse memory structure that initializes anything read with 0
// TODO: this needs to work with imem
#[derive(Default)]
pub struct ToyMemory {
    mem: HashMap<usize, u32>,
    fallthrough: Option<Arc<RwLock<ElfBackedMem>>>,
}

impl ToyMemory {
    pub fn set_fallthrough(&mut self, fallthrough: Arc<RwLock<ElfBackedMem>>) {
        self.fallthrough = Some(fallthrough);
    }
}

impl HasMemory for ToyMemory {
    fn read<const N: usize>(&mut self, addr: usize) -> Option<Arc<[u8; N]>> {
        assert!((N % 4 == 0) && N > 0, "word sized requests only");
        assert_eq!(addr & 0x3, 0, "misaligned load across word boundary");
        let words: Vec<_> = (addr..addr + N).step_by(4).map(|a| {
            (!self.mem.contains_key(&a)).then_some(()).and_then(|_| {
                if let Some(elf) = &self.fallthrough {
                    info!("fallthrough");
                    elf.write().unwrap().read::<4>(a).map(|r| u32::from_le_bytes(*r))
                } else {
                    None
                }
            })
            .or_else(|| Some(*self.mem.entry(a).or_insert(0u32))).unwrap()
        }).collect();

        let byte_array: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        Some(Arc::new(byte_array.try_into().unwrap()))
    }

    fn write<const N: usize>(&mut self, addr: usize, data: Arc<[u8; N]>) -> Result<(), String> {
        if N < 4 {
            assert!((addr % 3) + N - 1 < 4, "misaligned store across word boundary");
            let word_addr = addr >> 2 << 2;
            if !self.mem.contains_key(&word_addr) {
                let read_value = u32::from_le_bytes(*(self.read::<4>(addr >> 2 << 2).unwrap()));
                self.mem.insert(word_addr, read_value);
            }
            let curr = self.mem.entry(word_addr).or_insert(0u32);

            for i in 0..N {
                let shift = ((addr & 3) + i) * 8;
                if shift >= 32 {
                    return Err("sh across word boundary".into());
                }
                *curr &= !(0xFF << shift);
                *curr |= (data[i] as u32) << shift;
            }
            
            Ok(())
        } else {
            assert!((N % 4 == 0) && N > 0, "word sized requests only");
            (0..N).step_by(4).for_each(|a| {
                let write_slice = &data[a..a + 4];
                self.mem.insert(addr + a, u32::from_le_bytes(write_slice.try_into().unwrap()));
            });
            Ok(())
        }
    }
}

impl ToyMemory {
    pub fn reset(&mut self) {
        self.mem.clear();
    }
}
