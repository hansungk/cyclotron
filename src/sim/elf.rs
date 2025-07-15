use std::path::Path;
use std::{collections::HashMap, fs};
use std::sync::Arc;
use goblin::elf::{section_header, Elf};
use crate::base::mem::HasMemory;

pub struct ElfBackedMem {
    pub sections: HashMap<(usize, usize), Vec<u8>>,
}

impl HasMemory for ElfBackedMem {
    fn read<const N: usize>(&mut self, addr: usize) -> Option<Arc<[u8; N]>> {
        self.sections.iter().fold(None, |prev, (range, data)| {
            prev.or(((addr >= range.0) && (addr + N <= range.1)).then(|| {
                Arc::new(data[(addr - range.0)..(addr - range.0 + N)].try_into().unwrap())
            }))
        })
    }

    fn write<const N: usize>(&mut self, _addr: usize, _data: Arc<[u8; N]>) -> Result<(), String> {
        Err("elf backed memory cannot be written to".to_string())
    }
}

impl ElfBackedMem {
    pub fn new(path: &Path) -> ElfBackedMem {
        let mut me = ElfBackedMem {
            sections: Default::default()
        };
        me.load_path(path.as_ref()).expect(&format!("Elf file {:?} not found", path));
        me
    }

    pub fn read_inst(&mut self, addr: usize) -> Option<u64> {
        let slice = self.read::<8>(addr)?;
        u64::from_le_bytes(*slice).into()
    }

    pub fn load_path(&mut self, path: &Path) -> Result<(), String> {
        let data = fs::read(path).map_err(|e| format!("Failed to read file: {}", e))?;
        let elf = Elf::parse(&data).map_err(|e| format!("Failed to parse ELF file: {}", e))?;

        self.sections = HashMap::new();

        // Iterate over the ELF sections
        for section in &elf.section_headers {
            let start = section.sh_addr;
            let size = section.sh_size;
            if size > 0 {
                let end = start + size;
                let range = (start as usize, end as usize);

                // Extract the section bytes
                let offset = section.sh_offset as usize;
                let size = section.sh_size as usize;
                if section_header::SHT_NOBITS == section.sh_type {
                    // SHT_NOBITS sections are implicitly zeroed, not on the file
                    self.sections.insert(range, vec![0u8; size]);
                } else if offset + size <= data.len() {
                    let bytes = data[offset..offset + size].to_vec();
                    self.sections.insert(range, bytes);
                } else {
                    return Err(format!(
                            "Invalid section bounds: offset {} size {}",
                            offset, size
                    ));
                }
            }
        }

        Ok(())
    }
}
