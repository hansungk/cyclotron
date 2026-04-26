use std::collections::VecDeque;

use super::timeq::{
    Backpressure, Cycle, ServerConfig, ServiceRequest, ServiceResult, Ticket, TimedServer,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BankedArrayMemType {
    TwoPort,
    TwoReadOneWrite,
    Custom {
        read_ports_per_subbank: usize,
        write_ports_per_subbank: usize,
    },
}

impl BankedArrayMemType {
    pub fn read_ports_per_subbank(self) -> usize {
        match self {
            Self::TwoPort => 1,
            Self::TwoReadOneWrite => 2,
            Self::Custom {
                read_ports_per_subbank,
                ..
            } => read_ports_per_subbank.max(1),
        }
    }

    pub fn write_ports_per_subbank(self) -> usize {
        match self {
            Self::TwoPort => 1,
            Self::TwoReadOneWrite => 1,
            Self::Custom {
                write_ports_per_subbank,
                ..
            } => write_ports_per_subbank.max(1),
        }
    }
}

impl Default for BankedArrayMemType {
    fn default() -> Self {
        Self::TwoPort
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BankedArrayAddressMap {
    // subbank = word % num_subbanks, bank = (word / num_subbanks) % num_banks
    SubbankThenBank,
    // bank = word % num_banks, subbank = (word / num_banks) % num_subbanks
    BankThenSubbank,
}

impl Default for BankedArrayAddressMap {
    fn default() -> Self {
        Self::SubbankThenBank
    }
}

#[derive(Debug, Clone)]
pub struct BankedArrayConfig {
    pub num_banks: usize,
    pub num_subbanks: usize,
    pub word_bytes: u32,
    pub address_map: BankedArrayAddressMap,
    pub mem_type: BankedArrayMemType,
    pub read_server: ServerConfig,
    pub write_server: ServerConfig,
}

impl Default for BankedArrayConfig {
    fn default() -> Self {
        Self {
            num_banks: 1,
            num_subbanks: 1,
            word_bytes: 4,
            address_map: BankedArrayAddressMap::default(),
            mem_type: BankedArrayMemType::default(),
            read_server: ServerConfig {
                bytes_per_cycle: 1024,
                queue_capacity: 64,
                ..ServerConfig::default()
            },
            write_server: ServerConfig {
                bytes_per_cycle: 1024,
                queue_capacity: 64,
                ..ServerConfig::default()
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct BankedArrayRequest<T> {
    pub addr: u64,
    pub size_bytes: u32,
    pub is_write: bool,
    pub payload: T,
    pub bank_override: Option<usize>,
    pub subbank_override: Option<usize>,
}

impl<T> BankedArrayRequest<T> {
    pub fn new(addr: u64, size_bytes: u32, is_write: bool, payload: T) -> Self {
        Self {
            addr,
            size_bytes: size_bytes.max(1),
            is_write,
            payload,
            bank_override: None,
            subbank_override: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BankedArrayRejectReason {
    InvalidBank,
    InvalidSubbank,
    QueueFull,
    Busy,
}

#[derive(Debug, Clone)]
pub struct BankedArrayReject<T> {
    pub request: BankedArrayRequest<T>,
    pub retry_at: Cycle,
    pub reason: BankedArrayRejectReason,
}

#[derive(Debug, Clone)]
pub struct BankedArrayIssue {
    pub bank: usize,
    pub subbank: usize,
    pub ticket: Ticket,
}

#[derive(Debug, Clone)]
pub struct BankedArrayCompletion<T> {
    pub request: BankedArrayRequest<T>,
    pub bank: usize,
    pub subbank: usize,
    pub ticket: Ticket,
    pub completed_at: Cycle,
}

#[derive(Debug, Clone, Default)]
pub struct BankedArrayStats {
    pub issued: u64,
    pub read_issued: u64,
    pub write_issued: u64,
    pub completed: u64,
    pub read_completed: u64,
    pub write_completed: u64,
    pub queue_full_rejects: u64,
    pub busy_rejects: u64,
    pub invalid_target_rejects: u64,
}

#[derive(Debug, Clone)]
struct PendingOp<T> {
    request: BankedArrayRequest<T>,
    bank: usize,
    subbank: usize,
}

pub struct BankedArray<T> {
    config: BankedArrayConfig,
    read_ports: Vec<Vec<Vec<TimedServer<PendingOp<T>>>>>,
    write_ports: Vec<Vec<Vec<TimedServer<PendingOp<T>>>>>,
    read_rr_next: Vec<Vec<usize>>,
    write_rr_next: Vec<Vec<usize>>,
    completions: VecDeque<BankedArrayCompletion<T>>,
    stats: BankedArrayStats,
}

impl<T> BankedArray<T> {
    pub fn new(config: BankedArrayConfig) -> Result<Self, String> {
        validate_config(&config)?;
        let read_units = config.mem_type.read_ports_per_subbank();
        let write_units = config.mem_type.write_ports_per_subbank();

        let read_ports = (0..config.num_banks)
            .map(|_| {
                (0..config.num_subbanks)
                    .map(|_| {
                        (0..read_units)
                            .map(|_| TimedServer::new(config.read_server))
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        let write_ports = (0..config.num_banks)
            .map(|_| {
                (0..config.num_subbanks)
                    .map(|_| {
                        (0..write_units)
                            .map(|_| TimedServer::new(config.write_server))
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        Ok(Self {
            read_ports,
            write_ports,
            read_rr_next: vec![vec![0; config.num_subbanks]; config.num_banks],
            write_rr_next: vec![vec![0; config.num_subbanks]; config.num_banks],
            completions: VecDeque::new(),
            stats: BankedArrayStats::default(),
            config,
        })
    }

    pub fn config(&self) -> &BankedArrayConfig {
        &self.config
    }

    pub fn stats(&self) -> BankedArrayStats {
        self.stats.clone()
    }

    pub fn issue(
        &mut self,
        now: Cycle,
        request: BankedArrayRequest<T>,
    ) -> Result<BankedArrayIssue, BankedArrayReject<T>> {
        let (bank, subbank) = match self.resolve_target(&request) {
            Ok(v) => v,
            Err(reason) => {
                self.stats.invalid_target_rejects =
                    self.stats.invalid_target_rejects.saturating_add(1);
                return Err(BankedArrayReject {
                    request,
                    retry_at: now.saturating_add(1),
                    reason,
                });
            }
        };

        let op = PendingOp {
            request,
            bank,
            subbank,
        };
        let is_write = op.request.is_write;
        let size_bytes = op.request.size_bytes.max(1);

        let (server, rr_next) = if op.request.is_write {
            (
                &mut self.write_ports[bank][subbank],
                &mut self.write_rr_next[bank][subbank],
            )
        } else {
            (
                &mut self.read_ports[bank][subbank],
                &mut self.read_rr_next[bank][subbank],
            )
        };
        let unit_idx = pick_port_unit(server, rr_next);

        match server[unit_idx].try_enqueue(now, ServiceRequest::new(op, size_bytes)) {
            Ok(ticket) => {
                self.stats.issued = self.stats.issued.saturating_add(1);
                if is_write {
                    self.stats.write_issued = self.stats.write_issued.saturating_add(1);
                } else {
                    self.stats.read_issued = self.stats.read_issued.saturating_add(1);
                }
                Ok(BankedArrayIssue {
                    bank,
                    subbank,
                    ticket,
                })
            }
            Err(bp) => {
                let (retry_at, reason, request) = map_backpressure(now, bp);
                match reason {
                    BankedArrayRejectReason::QueueFull => {
                        self.stats.queue_full_rejects =
                            self.stats.queue_full_rejects.saturating_add(1);
                    }
                    BankedArrayRejectReason::Busy => {
                        self.stats.busy_rejects = self.stats.busy_rejects.saturating_add(1);
                    }
                    _ => {}
                }
                Err(BankedArrayReject {
                    request,
                    retry_at,
                    reason,
                })
            }
        }
    }

    pub fn collect_completions(&mut self, now: Cycle) {
        let mut done: Vec<(bool, ServiceResult<PendingOp<T>>)> = Vec::new();
        for bank in 0..self.config.num_banks {
            for subbank in 0..self.config.num_subbanks {
                for server in &mut self.read_ports[bank][subbank] {
                    server.service_ready(now, |result| {
                        done.push((false, result));
                    });
                }
                for server in &mut self.write_ports[bank][subbank] {
                    server.service_ready(now, |result| {
                        done.push((true, result));
                    });
                }
            }
        }
        for (is_write, result) in done {
            self.stats.completed = self.stats.completed.saturating_add(1);
            if is_write {
                self.stats.write_completed = self.stats.write_completed.saturating_add(1);
            } else {
                self.stats.read_completed = self.stats.read_completed.saturating_add(1);
            }
            self.completions.push_back(BankedArrayCompletion {
                request: result.payload.request,
                bank: result.payload.bank,
                subbank: result.payload.subbank,
                ticket: result.ticket,
                completed_at: now,
            });
        }
    }

    pub fn pop_completion(&mut self) -> Option<BankedArrayCompletion<T>> {
        self.completions.pop_front()
    }

    pub fn pending_completions(&self) -> usize {
        self.completions.len()
    }

    pub fn decode_addr(&self, addr: u64) -> (usize, usize) {
        let word = addr / self.config.word_bytes.max(1) as u64;
        match self.config.address_map {
            BankedArrayAddressMap::SubbankThenBank => {
                let subbank = (word % self.config.num_subbanks as u64) as usize;
                let bank = ((word / self.config.num_subbanks as u64) % self.config.num_banks as u64)
                    as usize;
                (bank, subbank)
            }
            BankedArrayAddressMap::BankThenSubbank => {
                let bank = (word % self.config.num_banks as u64) as usize;
                let subbank = ((word / self.config.num_banks as u64)
                    % self.config.num_subbanks as u64) as usize;
                (bank, subbank)
            }
        }
    }

    fn resolve_target(
        &self,
        request: &BankedArrayRequest<T>,
    ) -> Result<(usize, usize), BankedArrayRejectReason> {
        let (mut bank, mut subbank) = self.decode_addr(request.addr);
        if let Some(override_bank) = request.bank_override {
            bank = override_bank;
        }
        if let Some(override_subbank) = request.subbank_override {
            subbank = override_subbank;
        }
        if bank >= self.config.num_banks {
            return Err(BankedArrayRejectReason::InvalidBank);
        }
        if subbank >= self.config.num_subbanks {
            return Err(BankedArrayRejectReason::InvalidSubbank);
        }
        Ok((bank, subbank))
    }
}

fn validate_config(config: &BankedArrayConfig) -> Result<(), String> {
    if config.num_banks == 0 {
        return Err("num_banks must be > 0".to_string());
    }
    if config.num_subbanks == 0 {
        return Err("num_subbanks must be > 0".to_string());
    }
    if config.word_bytes == 0 {
        return Err("word_bytes must be > 0".to_string());
    }
    if config.mem_type.read_ports_per_subbank() == 0 {
        return Err("read_ports_per_subbank must be > 0".to_string());
    }
    if config.mem_type.write_ports_per_subbank() == 0 {
        return Err("write_ports_per_subbank must be > 0".to_string());
    }
    Ok(())
}

fn pick_port_unit<T>(servers: &[TimedServer<T>], rr_next: &mut usize) -> usize {
    if servers.is_empty() {
        return 0;
    }
    let start = *rr_next % servers.len();
    for offset in 0..servers.len() {
        let idx = (start + offset) % servers.len();
        if servers[idx].available_slots() > 0 {
            *rr_next = (idx + 1) % servers.len();
            return idx;
        }
    }
    *rr_next = (start + 1) % servers.len();
    start
}

fn map_backpressure<T>(
    now: Cycle,
    backpressure: Backpressure<PendingOp<T>>,
) -> (Cycle, BankedArrayRejectReason, BankedArrayRequest<T>) {
    match backpressure {
        Backpressure::QueueFull { request, .. } => (
            now.saturating_add(1),
            BankedArrayRejectReason::QueueFull,
            request.payload.request,
        ),
        Backpressure::Busy {
            request,
            available_at,
        } => (
            available_at.max(now.saturating_add(1)),
            BankedArrayRejectReason::Busy,
            request.payload.request,
        ),
    }
}
