use crate::base::mem::HasMemory;
use anyhow::anyhow;
use goblin::elf::{section_header, Elf};
use std::path::Path;
use std::{collections::HashMap, fs};

pub struct ElfBackedMem {
    pub sections: HashMap<(usize, usize), Vec<u8>>,
}

impl HasMemory for ElfBackedMem {
    fn read_impl(&self, addr: usize, n: usize) -> Result<&[u8], anyhow::Error> {
        for section in &self.sections {
            let range = section.0;
            let data = section.1;

            if addr >= range.0 && addr + n <= range.1 {
                let start = addr - range.0;
                let end = start + n;
                return Ok(&data[start..end]);
            }
        }

        Err(anyhow!("address not present in any section"))
    }

    fn write_impl(&mut self, _addr: usize, _data: &[u8]) -> Result<(), anyhow::Error> {
        Err(anyhow!("elf backed memory cannot be written to"))
    }
}

impl ElfBackedMem {
    pub fn new(path: &Path) -> ElfBackedMem {
        let mut me = ElfBackedMem {
            sections: Default::default(),
        };
        me.load_path(path.as_ref())
            .expect(&format!("Elf file {:?} not found", path));
        me
    }

    pub fn load_path(&mut self, path: &Path) -> Result<(), String> {
        let data = fs::read(path).map_err(|e| format!("Failed to read file: {}", e))?;
        let elf = Elf::parse(&data).map_err(|e| format!("Failed to parse ELF file: {}", e))?;

        self.sections = HashMap::new();

        // Iterate over the ELF sections
        for section in &elf.section_headers {
            let section_name = elf.shdr_strtab.get_at(section.sh_name).unwrap_or_default();
            let start = section.sh_addr;
            let size = section.sh_size;
            if size > 0 {
                let mut range = None;
                if section_name == ".args" {
                    //. map .args to the kernel arg address
                    let base = 0x7fff0000usize;
                    range = Some((base, base + size as usize));
                } else if start != 0 {
                    let end = start + size;
                    range = Some((start as usize, end as usize));
                }

                // Extract the section bytes
                let offset = section.sh_offset as usize;
                let size = section.sh_size as usize;
                if section_header::SHT_NOBITS == section.sh_type {
                    // SHT_NOBITS sections are implicitly zeroed, not on the file
                    if let Some(range) = range {
                        self.sections.insert(range, vec![0u8; size]);
                    }
                } else if offset + size <= data.len() {
                    let bytes = data[offset..offset + size].to_vec();
                    if let Some(range) = range {
                        self.sections.insert(range, bytes);
                    }
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
