//! Mac memory pressure via `memory_pressure` and `vm_stat`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Pressure {
    Normal,
    Warn,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryInfo {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_percent: f64,
    pub pressure: Pressure,
}

impl MemoryInfo {
    pub fn from_parts(total: u64, available: u64, free_percent: Option<f64>) -> Self {
        let total_f = total.max(1) as f64;
        let used_percent = ((1.0 - available as f64 / total_f) * 1000.0).round() / 10.0;
        let free_pct = free_percent.unwrap_or(100.0 - used_percent);
        let pressure = pressure_from_free_percent(free_pct);
        Self { total_bytes: total, available_bytes: available, used_percent, pressure }
    }
}

/// macOS reports "System-wide memory free percentage: NN%". Apple's own thresholds
/// are roughly: yellow below ~20 %, red below ~10 %.
pub fn pressure_from_free_percent(free: f64) -> Pressure {
    if free < 10.0 {
        Pressure::Critical
    } else if free < 20.0 {
        Pressure::Warn
    } else {
        Pressure::Normal
    }
}

pub fn parse_memory_pressure(output: &str) -> Option<f64> {
    output
        .lines()
        .find(|l| l.contains("free percentage"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().trim_end_matches('%').parse().ok())
}

/// Parse `vm_stat`; returns available bytes (free + inactive + speculative + purgeable).
pub fn parse_vm_stat(output: &str) -> u64 {
    let mut page: u64 = 16384;
    let mut avail: u64 = 0;
    for line in output.lines() {
        if let Some(rest) = line.split("page size of ").nth(1) {
            page = rest.split_whitespace().next().and_then(|s| s.parse().ok()).unwrap_or(page);
            continue;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        let n: u64 = v.trim().trim_end_matches('.').parse().unwrap_or(0);
        if matches!(k.trim(), "Pages free" | "Pages inactive" | "Pages speculative" | "Pages purgeable") {
            avail += n;
        }
    }
    avail * page
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    std::process::Command::new(cmd).args(args).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into())
}

/// Read live memory info on this machine (macOS; best effort elsewhere).
pub fn local_memory() -> MemoryInfo {
    #[cfg(target_os = "macos")]
    {
        let total = run("sysctl", &["-n", "hw.memsize"]).and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        let avail = run("vm_stat", &[]).map(|s| parse_vm_stat(&s)).unwrap_or(0);
        let free_pct = run("memory_pressure", &["-Q"]).and_then(|s| parse_memory_pressure(&s));
        MemoryInfo::from_parts(total, avail, free_pct)
    }
    #[cfg(target_os = "windows")]
    {
        // wmic is deprecated; PowerShell CIM is available on all supported Windows versions.
        let out = run(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "$o=Get-CimInstance Win32_OperatingSystem; \"$($o.TotalVisibleMemorySize) $($o.FreePhysicalMemory)\"",
            ],
        )
        .unwrap_or_default();
        let mut it = out.split_whitespace().filter_map(|s| s.parse::<u64>().ok());
        let total = it.next().unwrap_or(0) * 1024;
        let free = it.next().unwrap_or(0) * 1024;
        MemoryInfo::from_parts(total, free, None)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let text = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let get = |k: &str| -> u64 {
            text.lines()
                .find(|l| l.starts_with(k))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0)
                * 1024
        };
        let _ = run;
        MemoryInfo::from_parts(get("MemTotal:"), get("MemAvailable:"), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_memory_pressure() {
        let out = "The system has 17179869184 (1048576 pages with a page size of 16384).\n\
                   System-wide memory free percentage: 23%\n";
        assert_eq!(parse_memory_pressure(out), Some(23.0));
        assert_eq!(parse_memory_pressure("nothing"), None);
    }

    #[test]
    fn parses_vm_stat() {
        let out = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
                   Pages free:                               10000.\n\
                   Pages active:                            200000.\n\
                   Pages inactive:                           5000.\n\
                   Pages speculative:                        1000.\n\
                   Pages purgeable:                           500.\n";
        assert_eq!(parse_vm_stat(out), 16500 * 16384);
    }

    #[test]
    fn pressure_levels() {
        assert_eq!(pressure_from_free_percent(50.0), Pressure::Normal);
        assert_eq!(pressure_from_free_percent(15.0), Pressure::Warn);
        assert_eq!(pressure_from_free_percent(5.0), Pressure::Critical);
        let m = MemoryInfo::from_parts(16 << 30, 4 << 30, None);
        assert_eq!(m.used_percent, 75.0);
        assert_eq!(m.pressure, Pressure::Normal);
    }

    #[test]
    fn live_reading_is_sane() {
        let m = local_memory();
        assert!(m.used_percent >= 0.0 && m.used_percent <= 100.0);
    }
}
