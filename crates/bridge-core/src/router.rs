//! Smart router: decide whether a job runs on the Mac or the PC.
//!
//! Rules, in order:
//! 1. Redirect off / PC unreachable            -> Mac
//! 2. Light job                                 -> Mac
//! 3. Job needs more RAM than the Mac has free  -> PC (if the PC has room)
//! 4. Mac under critical memory pressure        -> PC
//! 5. Otherwise compare estimated times: Mac vs (PC compute + transfer + queue + wake),
//!    with a swap penalty on the Mac when it is already under memory pressure.
//!
//! Estimates start from built-in speed factors and are replaced by real measurements from the job
//! history (SQLite) once a tool has a few runs on each side.

use crate::detect::{Classification, JobKind};
use crate::memory::{MemoryInfo, Pressure};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Mac,
    Pc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteInput {
    pub tool: String,
    pub class: Classification,
    pub input_bytes: u64,
    pub mac_mem: MemoryInfo,
    /// PC free RAM in bytes (None if unknown).
    pub pc_free_ram: Option<u64>,
    pub pc_reachable: bool,
    pub pc_awake: bool,
    pub pc_queue_depth: u32,
    pub measured_mbps: f64,
    pub redirect_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub target: Target,
    pub reason: String,
    pub mac_est_secs: f64,
    pub pc_est_secs: f64,
}

/// How much faster the RTX 3090 PC is than an M1 Pro, by default, per job kind/tool.
pub fn default_speedup(tool: &str, kind: JobKind) -> f64 {
    match (tool, kind) {
        ("ffmpeg", _) => 4.0,
        ("blender", _) => 5.0,
        ("comfy", _) => 8.0,
        (_, JobKind::Gpu) => 6.0,
        (_, JobKind::RamHeavy) => 1.3,
        _ => 1.0,
    }
}

/// Default Mac seconds per MB of input (very rough starting point).
pub fn default_mac_secs_per_mb(tool: &str, kind: JobKind) -> f64 {
    match (tool, kind) {
        ("ffmpeg", _) => 0.25,
        (_, JobKind::Gpu) => 1.0,
        (_, JobKind::RamHeavy) => 0.2,
        _ => 0.01,
    }
}

const MIN_JOB_SECS: f64 = 5.0;
const WAKE_SECS: f64 = 25.0;
const AVG_JOB_SECS: f64 = 60.0;

pub struct History {
    conn: Connection,
}

impl History {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        Self::init(Connection::open(path)?)
    }

    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS jobs (
                id INTEGER PRIMARY KEY,
                ts INTEGER NOT NULL,
                tool TEXT NOT NULL,
                kind TEXT NOT NULL,
                target TEXT NOT NULL,
                input_bytes INTEGER NOT NULL,
                seconds REAL NOT NULL,
                exit_code INTEGER NOT NULL,
                name TEXT,
                pc_job_id TEXT,
                folder TEXT
            );
            CREATE INDEX IF NOT EXISTS jobs_tool ON jobs(tool, target);",
        )?;
        Ok(Self { conn })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        tool: &str,
        kind: JobKind,
        target: Target,
        input_bytes: u64,
        seconds: f64,
        exit_code: i32,
        name: &str,
        pc_job_id: Option<&str>,
        folder: Option<&str>,
    ) -> rusqlite::Result<()> {
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        self.conn.execute(
            "INSERT INTO jobs (ts, tool, kind, target, input_bytes, seconds, exit_code, name, pc_job_id, folder)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                ts,
                tool,
                serde_json::to_string(&kind).unwrap().trim_matches('"'),
                serde_json::to_string(&target).unwrap().trim_matches('"'),
                input_bytes as i64,
                seconds,
                exit_code,
                name,
                pc_job_id,
                folder
            ],
        )?;
        Ok(())
    }

    /// Median seconds-per-MB for the last 20 successful runs of `tool` on `target` (needs >= 3 runs).
    pub fn secs_per_mb(&self, tool: &str, target: Target) -> Option<f64> {
        let t = serde_json::to_string(&target).unwrap();
        let mut stmt = self
            .conn
            .prepare(
                "SELECT seconds, input_bytes FROM jobs WHERE tool=?1 AND target=?2 AND exit_code=0 AND input_bytes>0
                 ORDER BY ts DESC LIMIT 20",
            )
            .ok()?;
        let mut v: Vec<f64> = stmt
            .query_map(params![tool, t.trim_matches('"')], |r| {
                let s: f64 = r.get(0)?;
                let b: i64 = r.get(1)?;
                Ok(s / (b as f64 / 1_048_576.0).max(0.001))
            })
            .ok()?
            .filter_map(Result::ok)
            .collect();
        if v.len() < 3 {
            return None;
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        Some(v[v.len() / 2])
    }

    pub fn recent(&self, limit: u32) -> Vec<serde_json::Value> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT ts, tool, kind, target, input_bytes, seconds, exit_code, name, pc_job_id, folder
             FROM jobs ORDER BY ts DESC, id DESC LIMIT ?1",
        ) else {
            return vec![];
        };
        stmt.query_map(params![limit], |r| {
            Ok(serde_json::json!({
                "ts": r.get::<_, i64>(0)?, "tool": r.get::<_, String>(1)?, "kind": r.get::<_, String>(2)?,
                "target": r.get::<_, String>(3)?, "input_bytes": r.get::<_, i64>(4)?,
                "seconds": r.get::<_, f64>(5)?, "exit_code": r.get::<_, i32>(6)?,
                "name": r.get::<_, Option<String>>(7)?, "pc_job_id": r.get::<_, Option<String>>(8)?,
                "folder": r.get::<_, Option<String>>(9)?,
            }))
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }
}

fn gb(b: u64) -> f64 {
    b as f64 / (1u64 << 30) as f64
}

pub fn decide(input: &RouteInput, history: Option<&History>) -> Decision {
    let kind = input.class.kind;
    let mb = (input.input_bytes as f64 / 1_048_576.0).max(1.0);
    let tool = input.tool.as_str();

    let learned_mac = history.and_then(|h| h.secs_per_mb(tool, Target::Mac));
    let learned_pc = history.and_then(|h| h.secs_per_mb(tool, Target::Pc));
    let mac_rate = learned_mac.unwrap_or_else(|| default_mac_secs_per_mb(tool, kind));
    let pc_rate = learned_pc.unwrap_or_else(|| mac_rate / default_speedup(tool, kind));

    let mut mac_est = (mac_rate * mb).max(MIN_JOB_SECS);
    // Swap penalty: the Mac slows down a lot once it starts compressing / swapping memory.
    match input.mac_mem.pressure {
        Pressure::Warn => mac_est *= 1.5,
        Pressure::Critical => mac_est *= 3.0,
        Pressure::Normal => {}
    }
    if input.class.est_ram_bytes > input.mac_mem.available_bytes && kind != JobKind::Light {
        mac_est *= 2.0;
    }
    // Transfer: inputs up + outputs back (assume outputs ~ inputs), megabits/s -> MB/s.
    let mbps = input.measured_mbps.max(1.0);
    let transfer = 2.0 * mb * 8.0 / mbps;
    let queue = input.pc_queue_depth as f64 * AVG_JOB_SECS;
    let wake = if input.pc_awake { 0.0 } else { WAKE_SECS };
    let pc_est = (pc_rate * mb).max(MIN_JOB_SECS * 0.5) + transfer + queue + wake;

    let d = |target, reason: String| Decision { target, reason, mac_est_secs: mac_est, pc_est_secs: pc_est };

    if !input.redirect_enabled {
        return d(Target::Mac, "Redirecting is paused".into());
    }
    if !input.pc_reachable {
        return d(Target::Mac, "The PC can't be reached right now".into());
    }
    if kind == JobKind::Light {
        return d(Target::Mac, input.class.reason.clone());
    }
    let pc_has_room = input.pc_free_ram.map(|f| f > input.class.est_ram_bytes).unwrap_or(true);
    if input.class.est_ram_bytes > input.mac_mem.available_bytes && pc_has_room {
        return d(
            Target::Pc,
            format!(
                "Needs about {:.1} GB of memory; the Mac only has {:.1} GB free",
                gb(input.class.est_ram_bytes),
                gb(input.mac_mem.available_bytes)
            ),
        );
    }
    if input.mac_mem.pressure == Pressure::Critical && pc_has_room {
        return d(Target::Pc, "The Mac is running low on memory".into());
    }
    if pc_est < mac_est {
        d(
            Target::Pc,
            format!("{} — faster on the PC (~{:.0}s vs ~{:.0}s on the Mac)", input.class.reason, pc_est, mac_est),
        )
    } else {
        d(Target::Mac, format!("Quicker to run on the Mac (~{:.0}s vs ~{:.0}s via the PC)", mac_est, pc_est))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1 << 30;

    fn input(kind: JobKind, ram: u64, bytes: u64, free: u64, pressure: Pressure) -> RouteInput {
        RouteInput {
            tool: "ffmpeg".into(),
            class: Classification { kind, est_ram_bytes: ram, reason: "test".into() },
            input_bytes: bytes,
            mac_mem: MemoryInfo { total_bytes: 16 * GB, available_bytes: free, used_percent: 50.0, pressure },
            pc_free_ram: Some(28 * GB),
            pc_reachable: true,
            pc_awake: true,
            pc_queue_depth: 0,
            measured_mbps: 900.0,
            redirect_enabled: true,
        }
    }

    #[test]
    fn big_encode_goes_to_pc() {
        let d = decide(&input(JobKind::Gpu, GB, 2 * GB, 8 * GB, Pressure::Normal), None);
        assert_eq!(d.target, Target::Pc, "{d:?}");
        assert!(d.pc_est_secs < d.mac_est_secs);
    }

    #[test]
    fn light_and_paused_stay_on_mac() {
        assert_eq!(decide(&input(JobKind::Light, 0, GB, 8 * GB, Pressure::Normal), None).target, Target::Mac);
        let mut i = input(JobKind::Gpu, GB, 2 * GB, 8 * GB, Pressure::Normal);
        i.redirect_enabled = false;
        assert_eq!(decide(&i, None).target, Target::Mac);
        i.redirect_enabled = true;
        i.pc_reachable = false;
        assert_eq!(decide(&i, None).target, Target::Mac);
    }

    #[test]
    fn slow_wifi_tiny_job_stays_local() {
        let mut i = input(JobKind::RamHeavy, GB, 3 * GB, 8 * GB, Pressure::Normal);
        i.tool = "python".into();
        i.measured_mbps = 20.0; // slow Wi-Fi: 6 GB round trip would take ages
        assert_eq!(decide(&i, None).target, Target::Mac);
    }

    #[test]
    fn ram_heavy_beyond_free_goes_to_pc() {
        let mut i = input(JobKind::RamHeavy, 12 * GB, 10 << 20, 4 * GB, Pressure::Normal);
        i.measured_mbps = 20.0;
        let d = decide(&i, None);
        assert_eq!(d.target, Target::Pc);
        assert!(d.reason.contains("memory"));
        // ...unless the PC is also full
        i.pc_free_ram = Some(2 * GB);
        assert_ne!(decide(&i, None).reason, d.reason);
    }

    #[test]
    fn critical_pressure_goes_to_pc() {
        let mut i = input(JobKind::RamHeavy, GB, 1 << 20, 3 * GB, Pressure::Critical);
        i.tool = "npm".into();
        assert_eq!(decide(&i, None).target, Target::Pc);
    }

    #[test]
    fn asleep_pc_and_queue_add_time() {
        let base = decide(&input(JobKind::Gpu, GB, 500 << 20, 8 * GB, Pressure::Normal), None);
        let mut i = input(JobKind::Gpu, GB, 500 << 20, 8 * GB, Pressure::Normal);
        i.pc_awake = false;
        i.pc_queue_depth = 2;
        let d = decide(&i, None);
        assert!((d.pc_est_secs - base.pc_est_secs - WAKE_SECS - 2.0 * AVG_JOB_SECS).abs() < 0.01);
    }

    #[test]
    fn learns_from_history() {
        let h = History::in_memory().unwrap();
        // Pretend the PC turned out to be slow for this tool and the Mac fast.
        for _ in 0..5 {
            h.record("ffmpeg", JobKind::Gpu, Target::Pc, 100 << 20, 100.0, 0, "x", None, None).unwrap();
            h.record("ffmpeg", JobKind::Gpu, Target::Mac, 100 << 20, 10.0, 0, "x", None, None).unwrap();
        }
        assert!((h.secs_per_mb("ffmpeg", Target::Pc).unwrap() - 1.0).abs() < 1e-9);
        let d = decide(&input(JobKind::Gpu, GB, 2 * GB, 8 * GB, Pressure::Normal), Some(&h));
        assert_eq!(d.target, Target::Mac, "{d:?}");
        assert_eq!(h.recent(3).len(), 3);
        assert!(h.secs_per_mb("blender", Target::Pc).is_none());
    }
}
