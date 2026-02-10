use anyhow::bail;

/// Trait for simulated memories in Cyclotron.
pub trait HasMemory {
    fn read_impl(&self, addr: usize, n: usize) -> Result<&[u8], anyhow::Error>;
    fn read(&self, addr: usize, n: usize) -> Result<&[u8], anyhow::Error> {
        // cyclotron code itself must maintain these invariants
        assert!((n % 4 == 0) && n > 0, "word sized reads only");
        assert!(addr + n - 1 < (1 << 32), "32-bit address range");

        // program binary might try and do this, which we should flag as
        // correctness issue
        if addr & 0x3 != 0 {
            bail!("unaligned memory read of size {} @ {:#08x}", n, addr);
        }

        self.read_impl(addr, n)
    }
    fn read_n<const N: usize>(&self, addr: usize) -> Result<[u8; N], anyhow::Error> {
        self.read(addr, N).map(|slice| slice.try_into().unwrap())
    }

    fn write_impl(&mut self, addr: usize, data: &[u8]) -> Result<(), anyhow::Error>;
    fn write(&mut self, addr: usize, data: &[u8]) -> Result<(), anyhow::Error> {
        let n: usize = data.len();
        let alignment: usize;

        assert!(addr + n - 1 < (1 << 32), "32-bit address range");
        if n < 4 {
            assert!(n == 1 || n == 2, "subword stores must be byte or half");
            alignment = n;
        } else {
            assert!(n % 4 == 0, "stores larger than a word must be integer number of words");
            alignment = 4;
        }

        if addr % alignment != 0 {
            bail!("unaligned memory write of size {} @ {:#08x}", n, addr);
        }

        self.write_impl(addr, data)
    }
    fn write_n<const N: usize>(&mut self, addr: usize, data: [u8; N]) -> Result<(), anyhow::Error> {
        self.write(addr, data.as_slice())
    }
}
