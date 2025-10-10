//! This module allows Cyclotron to be used as a backend memory model within a Verilog testbench.
//! The memory interface it provides is kept separate from the memory that Cyclotron itself uses to produce the 
//! golden architectural trace (though both are initialized with the same contents).

use crate::{base::mem::HasMemory, sim::elf::ElfBackedMem};

/// Gigantic 4 GB vector to model memory space; relies on lazy allocation within OS to avoid actually 
/// causing memory pressure. Avoids hash-table lookup for every memory access
struct FlatMemory {
    bytes: Vec<u8>
}

impl HasMemory for FlatMemory {
    fn read<const N: usize>(&mut self, addr: usize) -> Option<[u8; N]> {
        assert!((N % 4 == 0) && N > 0, "word sized reads only");
        assert_eq!(addr & 0x3, 0, "aligned reads only");
        assert!(addr + N - 1 < (1 << 32), "32-bit address range");

        Some(self.bytes[addr..addr+N].try_into().unwrap())
    }

    fn write(&mut self, addr: usize, data: &[u8]) -> Result<(), String> {
        let n: usize = data.len();
        assert!(addr + n - 1 < (1 << 32), "32-bit address range");
        if n < 4 {
            assert!(
                (n == 1) ||
                (n == 2 && addr % n == 0),
                "aligned stores only"
            );
        } else {
            assert!(addr % 4 == 0, "aligned stores only");
            assert!(n % 4 == 0, "stores larger than a word must be integer number of words");
        }

        let bytes = &mut self.bytes[addr..addr+n];
        bytes.copy_from_slice(data);

        Ok(())
    }
}

impl FlatMemory {
    fn new() -> Self {
        let bytes = vec![0u8, 1 << 32];
        Self { bytes }
    }

    fn copy_elf(&mut self, elf: &ElfBackedMem) {
        for (section, data) in elf.sections.iter() {
            let (start, end) = *section;
            let bytes = &mut self.bytes[start..end];
            bytes.copy_from_slice(&data);
        }
    }
}

