use std::io::Write;

use crate::{base::mem::HasMemory, sim::{config::MemConfig, elf::ElfBackedMem}};

/// Gigantic 4 GB vector to model memory space; relies on lazy allocation within OS to avoid actually 
/// causing memory pressure. Avoids hash-table lookup for every memory access
#[derive(Debug, Clone)]
pub struct FlatMemory {
    bytes: Vec<u8>,
    config: Option<MemConfig>,
}

impl HasMemory for FlatMemory {
    fn read_impl(&self, addr: usize, n: usize) -> Result<&[u8], anyhow::Error> {
        Ok(self.bytes[addr..addr+n].try_into().unwrap())
    }

    fn write_impl(&mut self, addr: usize, data: &[u8]) -> Result<(), anyhow::Error> {
        // Check if this is a write to the I/O console address range
        if let Some(config) = self.config {
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
                return Ok(());
            }
        }

        let bytes = &mut self.bytes[addr..addr+data.len()];
        bytes.copy_from_slice(data);

        Ok(())
    }
}

impl FlatMemory {
    pub fn new(config: Option<MemConfig>) -> Self {
        let bytes = vec![0u8; 1 << 32];
        Self { bytes, config }
    }

    pub fn new_with_size(size: usize, config: Option<MemConfig>) -> Self {
        let bytes = vec![0u8; size];
        Self { bytes, config }
    }

    pub fn copy_elf(&mut self, elf: &ElfBackedMem) {
        for (section, data) in elf.sections.iter() {
            let (start, end) = *section;
            let bytes = &mut self.bytes[start..end];
            bytes.copy_from_slice(&data);
        }
    }
}
