use std::sync::Arc;
use crate::base::port::*;

pub trait HasMemory {
    fn read<const N: usize>(&mut self, addr: usize) -> Option<Arc<[u8; N]>>;

    fn write(&mut self, addr: usize, data: &Vec<u8>) -> Result<(), String>;
}

#[derive(Debug, Default, Clone)]
pub enum MemReqOp {
    #[default]
    Get,
    Put,
}

#[derive(Debug, Default, Clone)]
pub enum MemRespOp {
    #[default]
    Ack
}

#[derive(Debug, Default, Clone)]
pub struct MemRequest {
    pub address: usize,
    pub size: usize,
    pub op: MemReqOp,
    pub data: Option<Arc<[u8]>>
}

impl<D> Port<D, MemRequest> {
    pub fn read<const N: usize>(&mut self, addr: usize) -> bool {
        self.put(&MemRequest {
            address: addr,
            size: N,
            op: MemReqOp::Get,
            data: None,
        })
    }

    pub fn write<const N: usize>(&mut self, addr: usize, data: Arc<[u8; N]>) -> bool {
        self.put(&MemRequest {
            address: addr,
            size: N,
            op: MemReqOp::Put,
            data: Some(data),
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct MemResponse {
    pub op: MemRespOp,
    pub data: Option<Arc<[u8]>> // why should this be an Arc?
}

pub trait HasMemoryPorts {
    // returns a list of tuples, each of which is one req/resp channel
    fn get_ports(&self) -> Vec<(&Port<InputPort, MemRequest>, &Port<OutputPort, MemResponse>)>;
}
