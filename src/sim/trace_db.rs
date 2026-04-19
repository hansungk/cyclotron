use crate::muon::execute::Opcode;
use crate::sim::trace::Line;
use rusqlite::Connection;
use std::path::{Path, PathBuf};

pub struct TraceDb {
    conn: Connection,
}

impl TraceDb {
    pub fn new(db_path: &Path) -> Self {
        Self {
            conn: create_new_db_overwrite(db_path),
        }
    }

    pub fn record_inst_line(&self, cluster_id: u32, core_id: u32, line: &Line) {
        let rs1_string = encode_lane_data(&line.rs1_data);
        let rs2_string = encode_lane_data(&line.rs2_data);
        self.conn
            .execute(
                "INSERT INTO inst (cluster_id, core_id, warp, pc, lane_mask, has_rs1, rs1_id, rs1_data, has_rs2, rs2_id, rs2_data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                (
                    cluster_id,
                    core_id,
                    line.warp_id,
                    line.pc,
                    line.tmask,
                    inst_uses_rs1(line),
                    line.rs1_addr,
                    &rs1_string,
                    inst_uses_rs2(line),
                    line.rs2_addr,
                    &rs2_string,
                ),
            )
            .expect("failed to insert into inst");
    }
}

pub fn default_trace_db_path(trace_db_path_arg: Option<&Path>, elf_path: Option<&Path>) -> PathBuf {
    if let Some(path) = trace_db_path_arg {
        path.to_path_buf()
    } else if let Some(path) = elf_path {
        let trace_db_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.strip_suffix(".elf").unwrap_or(name).to_owned())
            .unwrap_or_else(|| "cyclotron_trace".to_owned());
        PathBuf::from(format!("{trace_db_name}.sqlite"))
    } else {
        PathBuf::from("cyclotron_trace.sqlite")
    }
}

pub fn create_new_db_overwrite(db_path: &Path) -> Connection {
    for suffix in ["", "-wal", "-shm"] {
        let path = format!("{}{}", db_path.display(), suffix);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => panic!("failed to remove existing trace database file {path}: {e}"),
        }
    }
    let conn = Connection::open(db_path).expect("failed to open sqlite trace database");

    conn.execute(
        "CREATE TABLE inst (
                    id    INTEGER PRIMARY KEY AUTOINCREMENT,
                    cluster_id INTEGER NOT NULL,
                    core_id    INTEGER NOT NULL,
                    warp       INTEGER NOT NULL,
                    pc         INTEGER NOT NULL,
                    lane_mask  INTEGER NOT NULL,
                    has_rs1    INTEGER NOT NULL,
                    rs1_id     INTEGER NOT NULL,
                    rs1_data   TEXT NOT NULL,
                    has_rs2    INTEGER NOT NULL,
                    rs2_id     INTEGER NOT NULL,
                    rs2_data   TEXT NOT NULL
                )",
        (),
    )
    .expect("failed to create inst table");

    conn.execute(
        "CREATE TABLE dmem (
                    id         INTEGER PRIMARY KEY AUTOINCREMENT,
                    cluster_id INTEGER NOT NULL,
                    core_id    INTEGER NOT NULL,
                    lane_id    INTEGER NOT NULL,
                    store      INTEGER NOT NULL CHECK (store IN (0,1)),
                    address    INTEGER NOT NULL,
                    size       INTEGER NOT NULL,
                    data       INTEGER NOT NULL
                )",
        (),
    )
    .expect("failed to create dmem table");

    conn.execute(
        "CREATE TABLE smem (
                    id         INTEGER PRIMARY KEY AUTOINCREMENT,
                    cluster_id INTEGER NOT NULL,
                    core_id    INTEGER NOT NULL,
                    lane_id    INTEGER NOT NULL,
                    store      INTEGER NOT NULL CHECK (store IN (0,1)),
                    address    INTEGER NOT NULL,
                    size       INTEGER NOT NULL,
                    data       INTEGER NOT NULL
                )",
        (),
    )
    .expect("failed to create smem table");

    conn
}

fn encode_lane_data(data: &[Option<u32>]) -> String {
    data.iter()
        .map(|value| value.unwrap_or(0).to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn inst_uses_rs1(line: &Line) -> bool {
    match line.opcode {
        Opcode::LOAD
        | Opcode::LOAD_FP
        | Opcode::OP_IMM
        | Opcode::STORE
        | Opcode::STORE_FP
        | Opcode::OP
        | Opcode::OP_FP
        | Opcode::MADD
        | Opcode::MSUB
        | Opcode::NM_SUB
        | Opcode::NM_ADD
        | Opcode::BRANCH
        | Opcode::JALR => true,
        Opcode::SYSTEM => matches!(line.f3, 1 | 2 | 3),
        Opcode::CUSTOM0 => matches!(sfu_key(line), 0b000_0000000 | 0b001_0000000 | 0b010_0000000 | 0b100_0000000 | 0b101_0000000),
        Opcode::CUSTOM2 => matches!(extended_opcode(line), Opcode::NU_INVOKE | Opcode::NU_PAYLOAD),
        _ => false,
    }
}

fn inst_uses_rs2(line: &Line) -> bool {
    match line.opcode {
        Opcode::STORE
        | Opcode::STORE_FP
        | Opcode::OP
        | Opcode::OP_FP
        | Opcode::MADD
        | Opcode::MSUB
        | Opcode::NM_SUB
        | Opcode::NM_ADD
        | Opcode::BRANCH => true,
        Opcode::CUSTOM0 => matches!(sfu_key(line), 0b001_0000000 | 0b100_0000000 | 0b101_0000000),
        Opcode::CUSTOM2 => extended_opcode(line) == Opcode::NU_INVOKE && line.rs2_addr != 0,
        _ => false,
    }
}

fn extended_opcode(line: &Line) -> u16 {
    line.opcode as u16 | ((line.opext as u16) << 7)
}

fn sfu_key(line: &Line) -> u16 {
    ((line.f3 as u16) << 7) | line.f7 as u16
}
