use std::collections::VecDeque;

use super::timeq::{Cycle, ServerConfig, ServiceRequest, Ticket, TimedServer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArbitrationPolicy {
    RoundRobin,
    LowestIndexFirst,
}

#[derive(Debug, Clone)]
pub enum Connectivity {
    Full,
    Matrix(Vec<Vec<bool>>),
}

impl Connectivity {
    fn allows(&self, input: usize, output: usize) -> bool {
        match self {
            Self::Full => true,
            Self::Matrix(matrix) => matrix
                .get(input)
                .and_then(|row| row.get(output))
                .copied()
                .unwrap_or(false),
        }
    }

    fn validate(&self, num_inputs: usize, num_outputs: usize) -> Result<(), String> {
        match self {
            Self::Full => Ok(()),
            Self::Matrix(matrix) => {
                if matrix.len() != num_inputs {
                    return Err(format!(
                        "connectivity matrix row count {} != num_inputs {}",
                        matrix.len(),
                        num_inputs
                    ));
                }
                if let Some((row_idx, row)) = matrix
                    .iter()
                    .enumerate()
                    .find(|(_, row)| row.len() != num_outputs)
                {
                    return Err(format!(
                        "connectivity matrix row {} width {} != num_outputs {}",
                        row_idx,
                        row.len(),
                        num_outputs
                    ));
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct InterconnectConfig {
    pub num_inputs: usize,
    pub num_outputs: usize,
    pub input_queue_capacity: usize,
    pub grants_per_output_per_cycle: usize,
    pub arbitration: ArbitrationPolicy,
    pub output_server: ServerConfig,
    pub connectivity: Connectivity,
}

impl InterconnectConfig {
    pub fn simple(num_inputs: usize, num_outputs: usize) -> Self {
        Self {
            num_inputs: num_inputs.max(1),
            num_outputs: num_outputs.max(1),
            ..Self::default()
        }
    }

    pub fn with_connectivity_matrix(mut self, matrix: Vec<Vec<bool>>) -> Self {
        self.connectivity = Connectivity::Matrix(matrix);
        self
    }

    fn validate(&self) -> Result<(), String> {
        if self.num_inputs == 0 {
            return Err("num_inputs must be > 0".to_string());
        }
        if self.num_outputs == 0 {
            return Err("num_outputs must be > 0".to_string());
        }
        if self.input_queue_capacity == 0 {
            return Err("input_queue_capacity must be > 0".to_string());
        }
        if self.grants_per_output_per_cycle == 0 {
            return Err("grants_per_output_per_cycle must be > 0".to_string());
        }
        self.connectivity
            .validate(self.num_inputs, self.num_outputs)?;
        Ok(())
    }
}

impl Default for InterconnectConfig {
    fn default() -> Self {
        Self {
            num_inputs: 1,
            num_outputs: 1,
            input_queue_capacity: 64,
            grants_per_output_per_cycle: 1,
            arbitration: ArbitrationPolicy::RoundRobin,
            output_server: ServerConfig {
                bytes_per_cycle: 1024,
                queue_capacity: 64,
                ..ServerConfig::default()
            },
            connectivity: Connectivity::Full,
        }
    }
}

#[derive(Debug, Clone)]
pub struct InterconnectRequest<T> {
    pub input: usize,
    pub output: usize,
    pub payload: T,
    pub size_bytes: u32,
}

impl<T> InterconnectRequest<T> {
    pub fn new(input: usize, output: usize, payload: T, size_bytes: u32) -> Self {
        Self {
            input,
            output,
            payload,
            size_bytes: size_bytes.max(1),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterconnectRejectReason {
    InvalidInput,
    InvalidOutput,
    Disconnected,
    InputQueueFull,
}

#[derive(Debug, Clone)]
pub struct InterconnectReject<T> {
    pub request: InterconnectRequest<T>,
    pub retry_at: Cycle,
    pub reason: InterconnectRejectReason,
}

#[derive(Debug, Clone)]
pub struct InterconnectCompletion<T> {
    pub request: InterconnectRequest<T>,
    pub ticket: Ticket,
    pub completed_at: Cycle,
}

#[derive(Debug, Clone, Default)]
pub struct InterconnectStats {
    pub enqueued: u64,
    pub granted: u64,
    pub completed: u64,
    pub rejected_invalid_input: u64,
    pub rejected_invalid_output: u64,
    pub rejected_disconnected: u64,
    pub rejected_input_queue_full: u64,
}

pub struct InterconnectArbiter<T> {
    config: InterconnectConfig,
    input_queues: Vec<VecDeque<InterconnectRequest<T>>>,
    output_servers: Vec<TimedServer<InterconnectRequest<T>>>,
    rr_next: Vec<usize>,
    completions: VecDeque<InterconnectCompletion<T>>,
    stats: InterconnectStats,
}

impl<T> InterconnectArbiter<T> {
    pub fn new(config: InterconnectConfig) -> Result<Self, String> {
        config.validate()?;
        let num_inputs = config.num_inputs;
        let num_outputs = config.num_outputs;

        Ok(Self {
            input_queues: (0..num_inputs)
                .map(|_| VecDeque::with_capacity(config.input_queue_capacity))
                .collect(),
            output_servers: (0..num_outputs)
                .map(|_| TimedServer::new(config.output_server))
                .collect(),
            rr_next: vec![0; num_outputs],
            completions: VecDeque::new(),
            stats: InterconnectStats::default(),
            config,
        })
    }

    pub fn config(&self) -> &InterconnectConfig {
        &self.config
    }

    pub fn stats(&self) -> InterconnectStats {
        self.stats.clone()
    }

    pub fn enqueue(
        &mut self,
        now: Cycle,
        request: InterconnectRequest<T>,
    ) -> Result<(), InterconnectReject<T>> {
        if request.input >= self.config.num_inputs {
            self.stats.rejected_invalid_input = self.stats.rejected_invalid_input.saturating_add(1);
            return Err(InterconnectReject {
                request,
                retry_at: now.saturating_add(1),
                reason: InterconnectRejectReason::InvalidInput,
            });
        }
        if request.output >= self.config.num_outputs {
            self.stats.rejected_invalid_output =
                self.stats.rejected_invalid_output.saturating_add(1);
            return Err(InterconnectReject {
                request,
                retry_at: now.saturating_add(1),
                reason: InterconnectRejectReason::InvalidOutput,
            });
        }
        if !self
            .config
            .connectivity
            .allows(request.input, request.output)
        {
            self.stats.rejected_disconnected = self.stats.rejected_disconnected.saturating_add(1);
            return Err(InterconnectReject {
                request,
                retry_at: now.saturating_add(1),
                reason: InterconnectRejectReason::Disconnected,
            });
        }
        let queue = &mut self.input_queues[request.input];
        if queue.len() >= self.config.input_queue_capacity {
            self.stats.rejected_input_queue_full =
                self.stats.rejected_input_queue_full.saturating_add(1);
            return Err(InterconnectReject {
                request,
                retry_at: now.saturating_add(1),
                reason: InterconnectRejectReason::InputQueueFull,
            });
        }
        queue.push_back(request);
        self.stats.enqueued = self.stats.enqueued.saturating_add(1);
        Ok(())
    }

    pub fn step(&mut self, now: Cycle) {
        for output in 0..self.config.num_outputs {
            let mut grants_left = self.config.grants_per_output_per_cycle;
            while grants_left > 0 {
                let Some(input_idx) = self.select_input_for_output(output) else {
                    break;
                };
                let Some(request) = self.input_queues[input_idx].pop_front() else {
                    break;
                };
                let size_bytes = request.size_bytes.max(1);

                let result = self.output_servers[output]
                    .try_enqueue(now, ServiceRequest::new(request, size_bytes));

                match result {
                    Ok(_ticket) => {
                        self.stats.granted = self.stats.granted.saturating_add(1);
                        grants_left = grants_left.saturating_sub(1);
                    }
                    Err(bp) => {
                        let request = bp.into_request().payload;
                        self.input_queues[input_idx].push_front(request);
                        break;
                    }
                }
            }
        }

        for output in 0..self.config.num_outputs {
            self.output_servers[output].service_ready(now, |result| {
                self.completions.push_back(InterconnectCompletion {
                    request: result.payload,
                    ticket: result.ticket,
                    completed_at: now,
                });
                self.stats.completed = self.stats.completed.saturating_add(1);
            });
        }
    }

    pub fn pop_completion(&mut self) -> Option<InterconnectCompletion<T>> {
        self.completions.pop_front()
    }

    pub fn pending_inputs(&self, input: usize) -> Option<usize> {
        self.input_queues.get(input).map(VecDeque::len)
    }

    pub fn pending_total(&self) -> usize {
        self.input_queues.iter().map(VecDeque::len).sum()
    }

    pub fn pending_completions(&self) -> usize {
        self.completions.len()
    }

    fn select_input_for_output(&mut self, output: usize) -> Option<usize> {
        let mut candidates: Vec<usize> = Vec::new();
        for input in 0..self.config.num_inputs {
            let Some(front) = self.input_queues[input].front() else {
                continue;
            };
            if front.output == output && self.config.connectivity.allows(input, output) {
                candidates.push(input);
            }
        }
        if candidates.is_empty() {
            return None;
        }

        match self.config.arbitration {
            ArbitrationPolicy::LowestIndexFirst => Some(*candidates.iter().min().unwrap_or(&0)),
            ArbitrationPolicy::RoundRobin => {
                let start = self.rr_next[output] % self.config.num_inputs;
                for offset in 0..self.config.num_inputs {
                    let idx = (start + offset) % self.config.num_inputs;
                    if candidates.contains(&idx) {
                        self.rr_next[output] = (idx + 1) % self.config.num_inputs;
                        return Some(idx);
                    }
                }
                Some(*candidates.iter().min().unwrap_or(&0))
            }
        }
    }
}
