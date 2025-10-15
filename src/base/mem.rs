pub trait HasMemory {
    fn read<const N: usize>(&mut self, addr: usize) -> Option<[u8; N]>;
    fn write(&mut self, addr: usize, data: &[u8]) -> Result<(), String>;
}