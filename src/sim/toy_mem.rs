use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::io::Write;
use crate::base::mem::HasMemory;
use crate::sim::config::MemConfig;
use crate::sim::elf::ElfBackedMem;

// a sparse memory structure that initializes anything read with 0
#[derive(Default)]
pub struct ToyMemory {
    mem: HashMap<usize, u32>,
    fallthrough: Option<Arc<RwLock<ElfBackedMem>>>,
    config: Option<MemConfig>,
}

impl ToyMemory {
    pub fn set_fallthrough(&mut self, fallthrough: Arc<RwLock<ElfBackedMem>>) {
        self.fallthrough = Some(fallthrough);
    }

    pub fn set_config(&mut self, config: MemConfig) {
        self.config = Some(config);
    }
}

impl HasMemory for ToyMemory {
    fn read<const N: usize>(&mut self, addr: usize) -> Option<Arc<[u8; N]>> {
        assert!((N % 4 == 0) && N > 0, "word sized requests only");
        assert_eq!(addr & 0x3, 0, "misaligned load across word boundary");
        let words: Vec<_> = (addr..addr + N).step_by(4).map(|a| {
            (!self.mem.contains_key(&a)).then_some(()).and_then(|_| {
                if let Some(elf) = &self.fallthrough {
                    let d = elf.write().unwrap().read::<4>(a).map(|r| u32::from_le_bytes(*r));
                    if d.is_some() {
                        self.mem.entry(a).or_insert(d.unwrap());
                    }
                    d
                } else {
                    None
                }
            })
            .or_else(|| Some(*self.mem.entry(a).or_insert(0u32))).unwrap()
        }).collect();

        let byte_array: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        Some(Arc::new(byte_array.try_into().unwrap()))
    }

    fn write(&mut self, addr: usize, data: &Vec<u8>) -> Result<(), String> {
        // Check if this is a write to the I/O console address range
        if let Some(config) = &self.config {
            if addr >= config.io_cout_addr && addr < config.io_cout_addr + config.io_cout_size {
                let bytes = &*data;
                if let Ok(text) = std::str::from_utf8(bytes) {
                    print!("{}", text);
                } else {
                    // Print as hex if not valid UTF-8
                    for &byte in bytes.iter() {
                        print!("{:02x}", byte);
                    }
                }
                std::io::stdout().flush().unwrap();
            }
        }

        let n: usize = data.len();
        if n < 4 {
            assert!((addr % 3) + n - 1 < 4, "misaligned store across word boundary");
            let word_addr = addr >> 2 << 2;
            if !self.mem.contains_key(&word_addr) {
                let read_value = u32::from_le_bytes(*(self.read::<4>(addr >> 2 << 2).unwrap()));
                self.mem.insert(word_addr, read_value);
            }
            let curr = self.mem.entry(word_addr).or_insert(0u32);

            for i in 0..n {
                let shift = ((addr & 3) + i) * 8;
                if shift >= 32 {
                    return Err("sh across word boundary".into());
                }
                *curr &= !(0xFF << shift);
                *curr |= (data[i] as u32) << shift;
            }
            
            Ok(())
        } else {
            assert!((n % 4 == 0) && n > 0, "word sized requests only");
            (0..n).step_by(4).for_each(|a| {
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

    pub fn read_byte(&mut self, addr: usize) -> Option<u8> {
        let word_addr = addr & !0x3;
        let byte_index = (addr & 0x3) as usize;

        let word_bytes = self.read::<4>(word_addr)?;
        Some(word_bytes[byte_index])
    }

    pub fn write_byte(&mut self, addr: usize, value: u8) -> Result<(), String> {
        let word_addr = addr & !0x3;
        let byte_index = (addr & 0x3) as usize;

        let mut word_bytes = self
            .read::<4>(word_addr)
            .map(|bytes| bytes.as_ref().to_vec())
            .unwrap_or_else(|| vec![0; 4]);

        word_bytes[byte_index] = value;

        self.write(word_addr, &word_bytes)
    }
}
