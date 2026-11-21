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
//! history (SQLite) once a job signature (or else its tool) has a few runs on a side. Both sides'
//! rates always describe the same jobs (see [`History::learned_rates`]), and the PC's measured
//! transfer overhead is added when it is more than the bandwidth model predicts.

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
    /// The PC's own estimate of the wait for a free GPU slot (agent 0.1.5+). Preferred over
    /// `pc_queue_depth` × a fixed guess.
    #[serde(default)]
    pub pc_queue_secs: Option<f64>,
    pub measured_mbps: f64,
    pub redirect_enabled: bool,
    /// From [`job_signature`]; empty means "just the tool".
    #[serde(default)]
    pub signature: String,
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
const MB: f64 = 1_048_576.0;
const WAKE_SECS: f64 = 25.0;
const AVG_JOB_SECS: f64 = 60.0;

/// Runs needed (per signature, else per tool) before measurements replace the defaults.
const MIN_RUNS: usize = 3;
/// How many recent runs the learned rate looks at.
const LEARN_WINDOW: u32 = 20;
/// Weight of each older run relative to the next newer one.
const RECENCY_DECAY: f64 = 0.85;
/// Rows written before this version stored the whole Mac-side round trip as a PC job's time.
const SCHEMA_VER: i64 = 2;
const MAX_SUB_KEY: usize = 64;

/// Input size in MB as the router counts it: at least 1 MB, so a tiny job costs its fixed time.
/// Learned rates and estimates must use the same measure, or small inputs inflate estimates.
pub fn billed_mb(bytes: u64) -> f64 {
    (bytes as f64 / MB).max(1.0)
}

/// A short key telling apart jobs of the same tool that cost very different amounts,
/// e.g. `python:train.py`, `ffmpeg:libx264`, `blender:render`, `npm:build`, `ollama:llama3`.
pub fn job_signature(tool: &str, args: &[String]) -> String {
    let tool = tool.to_ascii_lowercase();
    let base = tool.trim_end_matches(".exe");
    let file_name = |s: &str| Path::new(&s.replace('\\', "/")).file_name().map(|f| f.to_string_lossy().to_lowercase());
    let after =
        |flags: &[&str]| args.windows(2).find(|w| flags.contains(&w[0].as_str())).map(|w| w[1].to_ascii_lowercase());
    let mut positional = args.iter().map(String::as_str).filter(|a| !a.starts_with('-'));
    let sub = match base {
        t if t.starts_with("python") => {
            python_sub_key(args).and_then(|(module, s)| if module { Some(s) } else { file_name(&s) })
        }
        "ffmpeg" => after(&["-c:v", "-vcodec", "-codec:v"]).or_else(|| after(&["-c", "-codec"])),
        "blender" => after(&["-P", "--python"]).and_then(|s| file_name(&s)).or_else(|| Some("render".into())),
        "node" | "deno" | "bun" => positional.next().and_then(file_name),
        "npm" | "pnpm" | "yarn" => match positional.next() {
            Some("run" | "run-script") => positional.next().map(str::to_lowercase),
            other => other.map(str::to_lowercase),
        },
        "ollama" => positional.nth(1).map(str::to_lowercase),
        _ => None,
    };
    match sub {
        Some(s) if !s.is_empty() => format!("{base}:{}", s.chars().take(MAX_SUB_KEY).collect::<String>()),
        _ => base.to_string(),
    }
}

/// `(is_module, name)` for `python [opts] script.py` or `python [opts] -m module`; None for `-c` code.
fn python_sub_key(args: &[String]) -> Option<(bool, String)> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-c" => return None,
            "-m" => return it.next().map(|m| (true, m.to_ascii_lowercase())),
            "-W" | "-X" | "-Q" => {
                it.next();
            }
            s if s.starts_with('-') => {}
            s => return Some((false, s.to_string())),
        }
    }
    None
}

/// One finished job, as stored in the history.
#[derive(Debug, Clone)]
pub struct JobRecord<'a> {
    pub tool: &'a str,
    pub signature: &'a str,
    pub kind: JobKind,
    pub target: Target,
    pub input_bytes: u64,
    /// Wall-clock seconds seen from the Mac (for a PC job: run + queue + bringing outputs back).
    pub seconds: f64,
    /// Seconds the job's process actually ran (for a PC job: the agent's "duration").
    pub compute_secs: f64,
    pub queue_secs: f64,
    /// Seconds spent sending inputs to the PC and waiting for outputs to come back.
    pub transfer_secs: f64,
    /// The PC answered from its result cache without running anything.
    pub cached: bool,
    /// `compute_secs` is only an upper bound (an agent before 0.1.4 also counted collecting the
    /// outputs). Such rows are learned from only while there aren't enough exact ones.
    pub approx: bool,
    pub exit_code: i32,
    pub name: &'a str,
    pub pc_job_id: Option<&'a str>,
    pub folder: Option<&'a str>,
}

impl<'a> JobRecord<'a> {
    /// A plain run that took `seconds` of compute, with no queue, transfer or cache involved.
    pub fn new(
        tool: &'a str,
        kind: JobKind,
        target: Target,
        input_bytes: u64,
        seconds: f64,
        exit_code: i32,
        name: &'a str,
    ) -> Self {
        Self {
            tool,
            signature: tool,
            kind,
            target,
            input_bytes,
            seconds,
            compute_secs: seconds,
            queue_secs: 0.0,
            transfer_secs: 0.0,
            cached: false,
            approx: false,
            exit_code,
            name,
            pc_job_id: None,
            folder: None,
        }
    }
}

fn tag<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap().trim_matches('"').to_string()
}

/// Recency-weighted median: `rates[0]` is the newest run and weighs the most.
fn weighted_median(rates: &[f64]) -> Option<f64> {
    if rates.len() < MIN_RUNS {
        return None;
    }
    let mut v: Vec<(f64, f64)> = rates.iter().enumerate().map(|(i, &r)| (r, RECENCY_DECAY.powi(i as i32))).collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let half = v.iter().map(|x| x.1).sum::<f64>() / 2.0;
    let mut acc = 0.0;
    for (rate, w) in &v {
        acc += w;
        if acc >= half {
            return Some(*rate);
        }
    }
    v.last().map(|x| x.0)
}

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
        // The shims and the app open the same file, sometimes at the same moment.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
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
        let existing: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('jobs')")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        const ADDED: &[(&str, &str)] = &[
            ("signature", "TEXT"),
            ("compute_secs", "REAL"),
            ("queue_secs", "REAL"),
            ("transfer_secs", "REAL"),
            ("cached", "INTEGER NOT NULL DEFAULT 0"),
            ("approx", "INTEGER NOT NULL DEFAULT 0"),
            // Existing rows get 1: their PC times are round trips, not compute time.
            ("schema_ver", "INTEGER NOT NULL DEFAULT 1"),
        ];
        for (col, ty) in ADDED {
            if existing.iter().any(|c| c == col) {
                continue;
            }
            match conn.execute(&format!("ALTER TABLE jobs ADD COLUMN {col} {ty}"), []) {
                Err(e) if e.to_string().contains("duplicate column") => {} // another process got there first
                other => {
                    other?;
                }
            }
        }
        conn.execute("CREATE INDEX IF NOT EXISTS jobs_sig ON jobs(signature, target)", [])?;
        Ok(Self { conn })
    }

    pub fn record(&self, r: &JobRecord) -> rusqlite::Result<()> {
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        self.conn.execute(
            "INSERT INTO jobs (ts, tool, kind, target, input_bytes, seconds, exit_code, name, pc_job_id, folder,
                               signature, compute_secs, queue_secs, transfer_secs, cached, approx, schema_ver)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                ts,
                r.tool,
                tag(&r.kind),
                tag(&r.target),
                r.input_bytes as i64,
                r.seconds,
                r.exit_code,
                r.name,
                r.pc_job_id,
                r.folder,
                r.signature,
                r.compute_secs,
                r.queue_secs,
                r.transfer_secs,
                r.cached,
                r.approx,
                SCHEMA_VER
            ],
        )?;
        Ok(())
    }

    /// Learned compute seconds per MB on `target` alone: a recency-weighted median of the last runs
    /// with the same `signature` (needs 3), else of the same `tool` (needs 3), else None.
    /// Failed runs, cache hits and PC rows from before compute time was recorded are ignored.
    pub fn secs_per_mb(&self, tool: &str, signature: &str, target: Target) -> Option<f64> {
        self.median_rate(tool, Some(signature), target, None).or_else(|| self.median_rate(tool, None, target, None))
    }

    /// `(mac, pc)` learned rates for a job, taken so that both describe the same jobs:
    /// 1. If either side has 3 runs of this signature, signature rates only. A side without them
    ///    gets None (the defaults), never a tool-wide rate measured on other jobs.
    /// 2. Else tool-wide rates over the signatures both sides have run, if both have 3 such runs.
    /// 3. Else a tool-wide rate on one side, when the other side has none to mismatch it.
    pub fn learned_rates(&self, tool: &str, signature: &str) -> (Option<f64>, Option<f64>) {
        let mac = self.median_rate(tool, Some(signature), Target::Mac, None);
        let pc = self.median_rate(tool, Some(signature), Target::Pc, None);
        if mac.is_some() || pc.is_some() {
            return (mac, pc);
        }
        let mac = self.median_rate(tool, None, Target::Mac, Some(Target::Pc));
        let pc = self.median_rate(tool, None, Target::Pc, Some(Target::Mac));
        if mac.is_some() && pc.is_some() {
            return (mac, pc);
        }
        match (self.median_rate(tool, None, Target::Mac, None), self.median_rate(tool, None, Target::Pc, None)) {
            (Some(_), Some(_)) => (None, None),
            one_side => one_side,
        }
    }

    /// Typical seconds a PC job of this signature (else tool) spent pushing inputs and waiting for
    /// its outputs to come back, from the last runs (needs 3).
    pub fn overhead_secs(&self, tool: &str, signature: &str) -> Option<f64> {
        let query = |sig: Option<&str>| -> Option<f64> {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT transfer_secs FROM jobs
                     WHERE tool=?1 AND target='pc' AND exit_code=0 AND schema_ver>=?2 AND transfer_secs IS NOT NULL
                       AND (?3 IS NULL OR signature=?3)
                     ORDER BY ts DESC, id DESC LIMIT ?4",
                )
                .ok()?;
            let rows = stmt.query_map(params![tool, SCHEMA_VER, sig, LEARN_WINDOW], |r| r.get::<_, f64>(0)).ok()?;
            weighted_median(&rows.filter_map(Result::ok).collect::<Vec<_>>())
        };
        query(Some(signature)).or_else(|| query(None))
    }

    /// Weighted median of [`Self::rates`], from exact rows alone when there are enough of them.
    fn median_rate(&self, tool: &str, signature: Option<&str>, target: Target, paired: Option<Target>) -> Option<f64> {
        let rows = self.rates(tool, signature, target, paired)?;
        let exact: Vec<f64> = rows.iter().filter(|r| !r.1).map(|r| r.0).collect();
        if exact.len() >= MIN_RUNS {
            return weighted_median(&exact);
        }
        weighted_median(&rows.iter().map(|r| r.0).collect::<Vec<_>>())
    }

    /// `(seconds per MB, approx)` of recent usable runs, newest first. With `paired`, only runs of
    /// signatures that also have usable runs on that other side.
    fn rates(
        &self,
        tool: &str,
        signature: Option<&str>,
        target: Target,
        paired: Option<Target>,
    ) -> Option<Vec<(f64, bool)>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT COALESCE(compute_secs, seconds), input_bytes, approx FROM jobs
                 WHERE tool=?1 AND target=?2 AND exit_code=0 AND cached=0
                   AND (target<>'pc' OR schema_ver>=?3) AND (?4 IS NULL OR signature=?4)
                   AND (?6 IS NULL OR signature IN (
                         SELECT o.signature FROM jobs o
                         WHERE o.tool=?1 AND o.target=?6 AND o.exit_code=0 AND o.cached=0
                           AND (o.target<>'pc' OR o.schema_ver>=?3)))
                 ORDER BY ts DESC, id DESC LIMIT ?5",
            )
            .ok()?;
        let paired = paired.map(|t| tag(&t));
        let rows = stmt
            .query_map(params![tool, tag(&target), SCHEMA_VER, signature, LEARN_WINDOW, paired], |r| {
                let s: f64 = r.get(0)?;
                let b: i64 = r.get(1)?;
                Ok((s / billed_mb(b.max(0) as u64), r.get::<_, bool>(2)?))
            })
            .ok()?;
        Some(rows.filter_map(Result::ok).collect())
    }

    pub fn recent(&self, limit: u32) -> Vec<serde_json::Value> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT ts, tool, kind, target, input_bytes, seconds, exit_code, name, pc_job_id, folder,
                    signature, compute_secs, queue_secs, transfer_secs, cached, approx
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
                "folder": r.get::<_, Option<String>>(9)?, "signature": r.get::<_, Option<String>>(10)?,
                "compute_secs": r.get::<_, Option<f64>>(11)?, "queue_secs": r.get::<_, Option<f64>>(12)?,
                "transfer_secs": r.get::<_, Option<f64>>(13)?, "cached": r.get::<_, bool>(14)?,
                "approx": r.get::<_, bool>(15)?,
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
    let mb = billed_mb(input.input_bytes);
    let tool = input.tool.as_str();
    let sig = if input.signature.is_empty() { tool } else { input.signature.as_str() };

    // Both rates are compute time only (PC rows store the agent's run time), so the PC's transfer,
    // queue and wake costs are added exactly once below.
    let (learned_mac, learned_pc) = history.map(|h| h.learned_rates(tool, sig)).unwrap_or_default();
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
    // Transfer: inputs up + outputs back (assume outputs ~ inputs), megabits/s -> MB/s. Small jobs
    // are dominated by fixed costs (checking inputs, waiting for Syncthing), which past runs measured.
    let mbps = input.measured_mbps.max(1.0);
    let overhead = history.and_then(|h| h.overhead_secs(tool, sig)).unwrap_or(0.0);
    let transfer = (2.0 * mb * 8.0 / mbps).max(overhead);
    let queue = input.pc_queue_secs.unwrap_or(input.pc_queue_depth as f64 * AVG_JOB_SECS).max(0.0);
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
    // Memory guard the other way: don't send a job to a PC that can't hold it.
    if !pc_has_room && input.class.est_ram_bytes <= input.mac_mem.available_bytes {
        return d(
            Target::Mac,
            format!(
                "Needs about {:.1} GB of memory; the PC only has {:.1} GB free right now",
                gb(input.class.est_ram_bytes),
                gb(input.pc_free_ram.unwrap_or(0))
            ),
        );
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
            pc_queue_secs: None,
            measured_mbps: 900.0,
            redirect_enabled: true,
            signature: String::new(),
        }
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn run(tool: &str, target: Target, mb: u64, compute: f64) -> JobRecord<'_> {
        JobRecord::new(tool, JobKind::Gpu, target, mb << 20, compute, 0, "x")
    }

    #[test]
    fn real_queue_estimate_replaces_the_guess() {
        let mut i = input(JobKind::Gpu, GB, 2 * GB, 8 * GB, Pressure::Normal);
        i.pc_queue_depth = 1;
        let guessed = decide(&i, None).pc_est_secs;
        i.pc_queue_secs = Some(600.0);
        let real = decide(&i, None).pc_est_secs;
        assert!((real - guessed - (600.0 - AVG_JOB_SECS)).abs() < 0.01, "{guessed} {real}");
    }

    #[test]
    fn a_pc_without_room_is_not_used_when_the_mac_has_it() {
        let mut i = input(JobKind::Gpu, 6 * GB, 2 * GB, 10 * GB, Pressure::Normal);
        i.pc_free_ram = Some(2 * GB);
        let d = decide(&i, None);
        assert_eq!(d.target, Target::Mac, "{d:?}");
        assert!(d.reason.contains("the PC only has 2.0 GB free"), "{}", d.reason);
        // Neither has room: the PC is still tried (it has more memory to swap with).
        i.mac_mem.available_bytes = 3 * GB;
        i.pc_free_ram = Some(5 * GB);
        assert_eq!(decide(&i, None).target, Target::Pc);
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
        // The PC turned out slow for this tool and the Mac fast.
        for _ in 0..5 {
            h.record(&run("ffmpeg", Target::Pc, 100, 100.0)).unwrap();
            h.record(&run("ffmpeg", Target::Mac, 100, 10.0)).unwrap();
        }
        assert!((h.secs_per_mb("ffmpeg", "ffmpeg", Target::Pc).unwrap() - 1.0).abs() < 1e-9);
        let d = decide(&input(JobKind::Gpu, GB, 2 * GB, 8 * GB, Pressure::Normal), Some(&h));
        assert_eq!(d.target, Target::Mac, "{d:?}");
        assert_eq!(h.recent(3).len(), 3);
        assert!(h.secs_per_mb("blender", "blender", Target::Pc).is_none());
    }

    #[test]
    fn decide_switches_to_learned_rates_after_three_runs() {
        let h = History::in_memory().unwrap();
        let i = input(JobKind::Gpu, GB, 2 * GB, 8 * GB, Pressure::Normal);
        let default = decide(&i, Some(&h));
        assert_eq!(default.target, Target::Pc);
        assert_eq!(default.pc_est_secs, decide(&i, None).pc_est_secs);
        for n in 1..=3 {
            h.record(&run("ffmpeg", Target::Pc, 100, 200.0)).unwrap();
            let d = decide(&i, Some(&h));
            if n < 3 {
                assert_eq!(d.pc_est_secs, default.pc_est_secs, "still defaults after {n} runs");
            } else {
                // 2 s/MB of compute × 2048 MB, plus transfer once: no queue or round trip baked in.
                let transfer = 2.0 * 2048.0 * 8.0 / 900.0;
                assert!((d.pc_est_secs - (2.0 * 2048.0 + transfer)).abs() < 1e-6, "{d:?}");
                assert_eq!(d.target, Target::Mac, "{d:?}");
            }
        }
    }

    #[test]
    fn cached_failed_and_legacy_rows_are_not_learned() {
        let h = History::in_memory().unwrap();
        for _ in 0..3 {
            h.record(&JobRecord { cached: true, compute_secs: 0.0, ..run("ffmpeg", Target::Pc, 100, 0.0) }).unwrap();
            h.record(&JobRecord { exit_code: 1, ..run("ffmpeg", Target::Pc, 100, 1.0) }).unwrap();
            h.conn
                .execute(
                    "INSERT INTO jobs (ts, tool, kind, target, input_bytes, seconds, exit_code)
                     VALUES (1, 'ffmpeg', 'gpu', 'pc', 104857600, 900.0, 0)",
                    [],
                )
                .unwrap();
        }
        assert!(h.secs_per_mb("ffmpeg", "ffmpeg", Target::Pc).is_none());
        assert_eq!(h.recent(100).len(), 9, "legacy and cached rows stay visible");
        for _ in 0..3 {
            h.record(&run("ffmpeg", Target::Pc, 100, 50.0)).unwrap();
        }
        assert!((h.secs_per_mb("ffmpeg", "ffmpeg", Target::Pc).unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn prefers_same_signature_then_tool() {
        let h = History::in_memory().unwrap();
        for _ in 0..3 {
            h.record(&JobRecord { signature: "python:tiny.py", ..run("python", Target::Mac, 10, 1.0) }).unwrap();
        }
        // Not enough train.py runs yet: fall back to everything python did.
        h.record(&JobRecord { signature: "python:train.py", ..run("python", Target::Mac, 10, 100.0) }).unwrap();
        assert!((h.secs_per_mb("python", "python:train.py", Target::Mac).unwrap() - 0.1).abs() < 1e-9);
        for _ in 0..2 {
            h.record(&JobRecord { signature: "python:train.py", ..run("python", Target::Mac, 10, 100.0) }).unwrap();
        }
        assert!((h.secs_per_mb("python", "python:train.py", Target::Mac).unwrap() - 10.0).abs() < 1e-9);
        assert!((h.secs_per_mb("python", "python:tiny.py", Target::Mac).unwrap() - 0.1).abs() < 1e-9);
        assert!(h.secs_per_mb("python", "python:other.py", Target::Mac).is_some());
        assert!(h.secs_per_mb("python", "python:train.py", Target::Pc).is_none());
    }

    #[test]
    fn recent_runs_weigh_more() {
        let h = History::in_memory().unwrap();
        for _ in 0..10 {
            h.record(&run("blender", Target::Pc, 100, 100.0)).unwrap();
        }
        // After an upgrade the last five runs are 4x faster: a plain median of 15 would still say 1.0.
        for _ in 0..5 {
            h.record(&run("blender", Target::Pc, 100, 25.0)).unwrap();
        }
        assert_eq!(h.secs_per_mb("blender", "blender", Target::Pc), Some(0.25));
        assert_eq!(weighted_median(&[1.0, 2.0]), None);
        assert_eq!(weighted_median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(weighted_median(&[1.0, 1.0, 9.0, 9.0, 9.0]), Some(9.0));
        assert_eq!(weighted_median(&[1.0, 1.0, 1.0, 9.0, 9.0]), Some(1.0));
    }

    #[test]
    fn migrates_an_old_history_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE jobs (id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, tool TEXT NOT NULL,
                    kind TEXT NOT NULL, target TEXT NOT NULL, input_bytes INTEGER NOT NULL, seconds REAL NOT NULL,
                    exit_code INTEGER NOT NULL, name TEXT, pc_job_id TEXT, folder TEXT);
                 CREATE INDEX jobs_tool ON jobs(tool, target);",
            )
            .unwrap();
            for target in ["pc", "pc", "pc", "mac", "mac", "mac"] {
                c.execute(
                    "INSERT INTO jobs (ts, tool, kind, target, input_bytes, seconds, exit_code, name)
                     VALUES (1, 'ffmpeg', 'gpu', ?1, 1048576, 30.0, 0, 'old')",
                    [target],
                )
                .unwrap();
            }
        }
        for _ in 0..2 {
            let h = History::open(&path).unwrap();
            assert_eq!(h.recent(100).len(), 6);
            // Old PC rows were round trips: ignored. Old Mac rows were real local times: kept.
            assert!(h.secs_per_mb("ffmpeg", "ffmpeg:libx264", Target::Pc).is_none());
            assert_eq!(h.secs_per_mb("ffmpeg", "ffmpeg:libx264", Target::Mac), Some(30.0));
        }
        let h = History::open(&path).unwrap();
        h.record(&JobRecord { signature: "ffmpeg:libx264", ..run("ffmpeg", Target::Pc, 1, 2.0) }).unwrap();
        let row = &h.recent(1)[0];
        assert_eq!((row["signature"].as_str(), row["compute_secs"].as_f64()), (Some("ffmpeg:libx264"), Some(2.0)));
        assert_eq!(h.recent(100)[6]["compute_secs"], serde_json::Value::Null);
        let cols: i64 = h.conn.query_row("SELECT COUNT(*) FROM pragma_table_info('jobs')", [], |r| r.get(0)).unwrap();
        assert_eq!(cols, 18);
    }

    fn py(sig: &str, target: Target, kb: u64, compute: f64) -> JobRecord<'_> {
        JobRecord { signature: sig, ..JobRecord::new("python", JobKind::Gpu, target, kb << 10, compute, 0, "x") }
    }

    fn py_input(kb: u64, sig: &str) -> RouteInput {
        let mut i = input(JobKind::Gpu, GB, kb << 10, 8 * GB, Pressure::Normal);
        i.tool = "python".into();
        i.signature = sig.into();
        i
    }

    #[test]
    fn small_inputs_are_measured_the_way_decide_bills_them() {
        let h = History::in_memory().unwrap();
        // A 3 s torch script with an 8 KB input, run on the Mac while the PC was away.
        for _ in 0..3 {
            h.record(&py("python:infer.py", Target::Mac, 8, 3.0)).unwrap();
        }
        assert_eq!(h.secs_per_mb("python", "python:infer.py", Target::Mac), Some(3.0));
        let mut i = py_input(8, "python:infer.py");
        i.pc_awake = false;
        i.pc_queue_depth = 2;
        let d = decide(&i, Some(&h));
        assert_eq!((d.target, d.mac_est_secs), (Target::Mac, MIN_JOB_SECS), "{d:?}");
        assert!(d.pc_est_secs < 150.0, "{d:?}");
        // Zero-byte jobs are billed as 1 MB too, so they are learned instead of skipped.
        for _ in 0..3 {
            h.record(&py("python", Target::Mac, 0, 2.0)).unwrap();
        }
        assert_eq!(h.secs_per_mb("python", "python", Target::Mac), Some(2.0));
    }

    #[test]
    fn both_rates_describe_the_same_jobs() {
        let h = History::in_memory().unwrap();
        for _ in 0..3 {
            h.record(&py("python:train.py", Target::Pc, 100 << 10, 200.0)).unwrap();
            h.record(&py("python:infer_small.py", Target::Mac, 100 << 10, 5.0)).unwrap();
        }
        // Only the PC ran train.py: the Mac side keeps its default, not infer_small.py's rate.
        assert_eq!(h.learned_rates("python", "python:train.py"), (None, Some(2.0)));
        let d = decide(&py_input(100 << 10, "python:train.py"), Some(&h));
        assert_eq!(d.mac_est_secs, 100.0, "the default 1 s/MB, not 0.05: {d:?}");
        // A script neither side ran, and no script both sides ran: the two tool rates are other jobs.
        assert_eq!(h.learned_rates("python", "python:eval.py"), (None, None));
        // Once both sides have run the same script, tool rates come from that script only.
        for _ in 0..3 {
            h.record(&py("python:shared.py", Target::Mac, 10 << 10, 10.0)).unwrap();
            h.record(&py("python:shared.py", Target::Pc, 10 << 10, 1.0)).unwrap();
        }
        assert_eq!(h.learned_rates("python", "python:eval.py"), (Some(1.0), Some(0.1)));
        // One side alone has data for the tool: nothing to mismatch it with.
        for _ in 0..3 {
            h.record(&JobRecord { tool: "node", ..py("node:a.js", Target::Pc, 1 << 10, 4.0) }).unwrap();
        }
        assert_eq!(h.learned_rates("node", "node:b.js"), (None, Some(4.0)));
    }

    #[test]
    fn measured_overhead_counts_for_small_pc_jobs() {
        let h = History::in_memory().unwrap();
        for _ in 0..3 {
            h.record(&JobRecord { transfer_secs: 6.0, ..py("python:x.py", Target::Pc, 200, 4.0) }).unwrap();
            h.record(&py("python:x.py", Target::Mac, 200, 8.0)).unwrap();
        }
        assert_eq!(h.overhead_secs("python", "python:x.py"), Some(6.0));
        assert_eq!(h.overhead_secs("python", "python:other.py"), Some(6.0), "tool-wide fallback");
        let d = decide(&py_input(200, "python:x.py"), Some(&h));
        // 4 s of compute plus 6 s of measured overhead, not 4 s plus a 0.02 s bandwidth guess.
        assert!((d.pc_est_secs - 10.0).abs() < 1e-9, "{d:?}");
        assert_eq!((d.target, d.mac_est_secs), (Target::Mac, 8.0), "{d:?}");
        // Big inputs: the bandwidth model is larger and wins.
        let d = decide(&py_input(1 << 20, "python:x.py"), Some(&h));
        let transfer = 2.0 * 1024.0 * 8.0 / 900.0;
        assert!((d.pc_est_secs - (4.0 * 1024.0 + transfer)).abs() < 1e-6, "{d:?}");
    }

    #[test]
    fn approximate_rows_count_only_until_exact_ones_exist() {
        let h = History::in_memory().unwrap();
        for _ in 0..3 {
            h.record(&JobRecord { approx: true, ..run("ffmpeg", Target::Pc, 100, 300.0) }).unwrap();
        }
        assert_eq!(h.secs_per_mb("ffmpeg", "ffmpeg", Target::Pc), Some(3.0));
        for _ in 0..3 {
            h.record(&run("ffmpeg", Target::Pc, 100, 100.0)).unwrap();
        }
        // Newer old-agent rows still don't outvote three exact ones.
        h.record(&JobRecord { approx: true, ..run("ffmpeg", Target::Pc, 100, 300.0) }).unwrap();
        assert_eq!(h.secs_per_mb("ffmpeg", "ffmpeg", Target::Pc), Some(1.0));
        assert_eq!(h.recent(1)[0]["approx"], true);
    }

    #[test]
    fn signatures() {
        let sig = |tool: &str, a: &[&str]| job_signature(tool, &s(a));
        assert_eq!(sig("python3", &["-u", "scripts/Train.py", "--epochs", "3"]), "python3:train.py");
        assert_eq!(
            sig("python", &["-W", "ignore", "-m", "torch.distributed.run", "x.py"]),
            "python:torch.distributed.run"
        );
        assert_eq!(sig("python", &["-c", "print(1)"]), "python");
        assert_eq!(sig("ffmpeg", &["-i", "a.mov", "-c:a", "aac", "-c:v", "libx264", "o.mp4"]), "ffmpeg:libx264");
        assert_eq!(sig("ffmpeg", &["-i", "a.mov", "-c", "copy", "o.mp4"]), "ffmpeg:copy");
        assert_eq!(sig("ffmpeg", &["-i", "a.mov", "o.mp4"]), "ffmpeg");
        assert_eq!(sig("blender", &["-b", "scene.blend", "-P", r"C:\x\bake.py"]), "blender:bake.py");
        assert_eq!(sig("blender", &["-b", "scene.blend", "-a"]), "blender:render");
        assert_eq!(sig("node", &["--max-old-space-size=8000", "build.mjs"]), "node:build.mjs");
        assert_eq!(sig("npm", &["run", "build"]), "npm:build");
        assert_eq!(sig("pnpm", &["test"]), "pnpm:test");
        assert_eq!(sig("ollama", &["run", "llama3", "hi"]), "ollama:llama3");
        assert_eq!(sig("Docker", &["build", "."]), "docker");
    }
}
