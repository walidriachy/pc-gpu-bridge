//! Classify a command (GPU-heavy, RAM-heavy, light) and estimate its RAM needs.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// Benefits from the RTX 3090 (encodes, AI inference, renders).
    Gpu,
    /// Needs lots of memory (data processing, containers, big builds, local LLMs).
    RamHeavy,
    /// Cheap: keep on the Mac.
    Light,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Classification {
    pub kind: JobKind,
    /// Rough peak RAM estimate in bytes.
    pub est_ram_bytes: u64,
    pub reason: String,
}

const GB: u64 = 1 << 30;

const GPU_PY: &[&str] = &[
    "torch",
    "diffusers",
    "whisper",
    "faster_whisper",
    "comfy",
    "transformers",
    "tensorflow",
    "jax",
    "cupy",
    "onnxruntime",
    "ultralytics",
    "xformers",
    "accelerate",
];
const RAM_PY: &[&str] = &["pandas", "numpy", "polars", "dask", "sklearn", "scipy", "pyarrow", "xarray", "duckdb"];

/// Programs PC Bridge knows how to redirect.
pub const SHIM_TOOLS: &[&str] =
    &["ffmpeg", "ffprobe", "python", "python3", "blender", "comfy", "docker", "node", "npm", "pnpm", "npx"];

/// Tools that are only redirected when the project opts in with a `.pcbridge` file.
pub const OPT_IN_TOOLS: &[&str] = &["docker", "node", "npm", "pnpm", "npx"];

/// Scan Python source text for GPU / RAM-heavy imports and explicit markers.
pub fn classify_python_source(src: &str) -> Option<(JobKind, String)> {
    let lower = src.to_lowercase();
    if lower.contains("# pcbridge: gpu") {
        return Some((JobKind::Gpu, "marked '# pcbridge: gpu'".into()));
    }
    if lower.contains("# pcbridge: local") {
        return None;
    }
    let mut ram_hit = None;
    for stmt in src.lines().flat_map(|l| l.split(';')) {
        let t = stmt.split('#').next().unwrap_or("").trim();
        let modules: Vec<&str> = if let Some(r) = t.strip_prefix("import ") {
            r.split(',').filter_map(|m| m.trim().split([' ', '.']).next()).collect()
        } else if let Some(r) = t.strip_prefix("from ") {
            r.split([' ', '.']).next().into_iter().collect()
        } else {
            continue;
        };
        for m in modules {
            if GPU_PY.contains(&m) {
                return Some((JobKind::Gpu, format!("uses {m}")));
            }
            if RAM_PY.contains(&m) && ram_hit.is_none() {
                ram_hit = Some(m.to_string());
            }
        }
    }
    if lower.contains("# pcbridge: ram") {
        return Some((JobKind::RamHeavy, "marked '# pcbridge: ram'".into()));
    }
    ram_hit.map(|m| (JobKind::RamHeavy, format!("uses {m}")))
}

/// Project opt-in file: `.pcbridge` in the project folder or any parent, e.g.
/// ```text
/// offload = ["npm", "pnpm", "docker"]
/// ```
pub fn project_opt_ins(cwd: &Path) -> Vec<String> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        let f = d.join(".pcbridge");
        if let Ok(text) = std::fs::read_to_string(&f) {
            return parse_opt_ins(&text);
        }
        dir = d.parent();
    }
    vec![]
}

pub fn parse_opt_ins(text: &str) -> Vec<String> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("offload") {
            let rest = rest.trim_start().trim_start_matches('=').trim();
            return rest
                .trim_matches(|c| c == '[' || c == ']')
                .split(',')
                .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    vec![]
}

fn total_size(paths: &[PathBuf]) -> u64 {
    paths.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum()
}

/// Parse a model size like "llama3:70b" or "qwen2.5-14b-instruct" -> billions of params.
pub fn model_billions(name: &str) -> Option<f64> {
    let re = regex::Regex::new(r"(?i)(\d+(?:\.\d+)?)\s*b\b").unwrap();
    re.captures(name).and_then(|c| c[1].parse().ok())
}

/// Classify `tool args...` run in `cwd`. `inputs` are existing files referenced by the args.
pub fn classify(tool: &str, args: &[String], cwd: &Path, inputs: &[PathBuf]) -> Classification {
    let tool = tool.to_lowercase();
    let in_bytes = total_size(inputs);
    let c = |kind, ram: u64, reason: &str| Classification { kind, est_ram_bytes: ram, reason: reason.into() };
    let sub = args.iter().find(|a| !a.starts_with('-')).map(|s| s.as_str()).unwrap_or("");

    match tool.as_str() {
        "ffmpeg" => {
            if args.iter().any(|a| a == "-version" || a == "-h" || a == "-codecs" || a == "-encoders") {
                return c(JobKind::Light, 0, "ffmpeg info command");
            }
            c(JobKind::Gpu, GB + in_bytes / 4, "video processing (NVIDIA encoder on the PC)")
        }
        "ffprobe" => c(JobKind::Light, 0, "ffprobe is quick: runs locally"),
        "blender" => c(JobKind::Gpu, 4 * GB, "3D render"),
        "comfy" => c(JobKind::Gpu, 8 * GB, "image/video generation"),
        "python" | "python3" => {
            let src = python_source(args, cwd);
            match src.as_deref().and_then(classify_python_source) {
                Some((JobKind::Gpu, why)) => c(JobKind::Gpu, 6 * GB + in_bytes * 2, &why),
                Some((JobKind::RamHeavy, why)) => c(JobKind::RamHeavy, (2 * GB).max(in_bytes * 4), &why),
                _ => c(JobKind::Light, 0, "ordinary Python script: runs locally"),
            }
        }
        "docker" => match sub {
            "build" | "run" | "compose" | "buildx" => c(JobKind::RamHeavy, 4 * GB, "Docker container work"),
            _ => c(JobKind::Light, 0, "docker housekeeping command"),
        },
        "npm" | "pnpm" => {
            let script = args.iter().filter(|a| !a.starts_with('-')).nth(1).map(|s| s.as_str()).unwrap_or("");
            let heavy = matches!(sub, "test" | "t" | "build")
                || (sub == "run" && (script.contains("build") || script.contains("test") || script.contains("e2e")));
            if heavy {
                c(JobKind::RamHeavy, 4 * GB, "large JavaScript build / test suite")
            } else {
                c(JobKind::Light, 0, "quick npm/pnpm command: runs locally")
            }
        }
        "npx" => {
            if matches!(sub, "next" | "jest" | "vitest" | "playwright" | "webpack" | "tsc") {
                c(JobKind::RamHeavy, 4 * GB, "large JavaScript build / test suite")
            } else {
                c(JobKind::Light, 0, "npx helper: runs locally")
            }
        }
        "node" => {
            if args.iter().any(|a| a.contains("max-old-space-size")) {
                c(JobKind::RamHeavy, 8 * GB, "node with a large memory limit")
            } else {
                c(JobKind::Light, 0, "node script: runs locally")
            }
        }
        "ollama" => {
            let b = args.iter().find_map(|a| model_billions(a)).unwrap_or(8.0);
            c(JobKind::Gpu, (b * 0.6 * GB as f64) as u64 + GB, "local LLM")
        }
        _ => c(JobKind::Light, 0, "unknown tool: runs locally"),
    }
}

/// Find the Python source being run: a script file, `-c CODE`, or `-m module`.
pub fn python_source(args: &[String], cwd: &Path) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-c" => return it.next().cloned(),
            "-m" => return it.next().map(|m| format!("import {m}")),
            s if s.starts_with('-') => continue,
            s => {
                let p = cwd.join(s);
                return std::fs::read_to_string(p).ok();
            }
        }
    }
    None
}

/// Swap CPU H.264/H.265 encoders for NVIDIA NVENC when it is safe to do so.
/// Returns `None` when nothing was changed (or when x264/x265-only options make it unsafe).
pub fn ffmpeg_to_nvenc(args: &[String]) -> Option<Vec<String>> {
    let unsafe_opts = ["-x264-params", "-x265-params", "-x264opts", "-tune", "-profile:v", "-pix_fmt"];
    if args.iter().any(|a| unsafe_opts.contains(&a.as_str())) {
        return None;
    }
    let mut out = Vec::with_capacity(args.len());
    let mut changed = false;
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        let is_codec = matches!(a.as_str(), "-c:v" | "-vcodec" | "-codec:v");
        if is_codec {
            if let Some(v) = it.peek() {
                let repl = match v.as_str() {
                    "libx264" => Some("h264_nvenc"),
                    "libx265" => Some("hevc_nvenc"),
                    _ => None,
                };
                if let Some(r) = repl {
                    out.push(a.clone());
                    out.push(r.into());
                    it.next();
                    changed = true;
                    continue;
                }
            }
        }
        out.push(a.clone());
    }
    if !changed {
        return None;
    }
    // Translate quality/preset flags to NVENC equivalents.
    let mut fixed = Vec::with_capacity(out.len());
    let mut it = out.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-crf" => {
                fixed.push("-cq".into());
                if let Some(v) = it.next() {
                    fixed.push(v);
                }
            }
            "-preset" => {
                fixed.push("-preset".into());
                if let Some(v) = it.next() {
                    let p = match v.as_str() {
                        "ultrafast" | "superfast" => "p1",
                        "veryfast" => "p2",
                        "faster" => "p3",
                        "fast" => "p4",
                        "medium" => "p5",
                        "slow" => "p6",
                        "slower" | "veryslow" | "placebo" => "p7",
                        other => other,
                    };
                    fixed.push(p.to_string());
                }
            }
            _ => fixed.push(a),
        }
    }
    Some(fixed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn python_detection() {
        assert_eq!(classify_python_source("import torch\nx=1").unwrap().0, JobKind::Gpu);
        assert_eq!(classify_python_source("from diffusers import X").unwrap().0, JobKind::Gpu);
        assert_eq!(classify_python_source("import os, whisper").unwrap().0, JobKind::Gpu);
        assert_eq!(classify_python_source("import pandas as pd").unwrap().0, JobKind::RamHeavy);
        assert_eq!(classify_python_source("# pcbridge: gpu\nprint(1)").unwrap().0, JobKind::Gpu);
        assert_eq!(classify_python_source("# pcbridge: ram\nprint(1)").unwrap().0, JobKind::RamHeavy);
        assert!(classify_python_source("import torch # pcbridge: local").is_none());
        assert!(classify_python_source("import os\nprint('torch')").is_none());
        // GPU wins over RAM when both present
        assert_eq!(classify_python_source("import numpy\nimport torch").unwrap().0, JobKind::Gpu);
    }

    #[test]
    fn classify_tools() {
        let cwd = Path::new("/tmp");
        assert_eq!(classify("ffmpeg", &s(&["-i", "a.mp4", "b.mp4"]), cwd, &[]).kind, JobKind::Gpu);
        assert_eq!(classify("ffmpeg", &s(&["-version"]), cwd, &[]).kind, JobKind::Light);
        assert_eq!(classify("ffprobe", &s(&["a.mp4"]), cwd, &[]).kind, JobKind::Light);
        assert_eq!(classify("python3", &s(&["-c", "import torch"]), cwd, &[]).kind, JobKind::Gpu);
        assert_eq!(classify("python", &s(&["-m", "whisper", "a.mp3"]), cwd, &[]).kind, JobKind::Gpu);
        assert_eq!(classify("python", &s(&["-c", "print(1)"]), cwd, &[]).kind, JobKind::Light);
        assert_eq!(classify("docker", &s(&["build", "."]), cwd, &[]).kind, JobKind::RamHeavy);
        assert_eq!(classify("docker", &s(&["ps"]), cwd, &[]).kind, JobKind::Light);
        assert_eq!(classify("npm", &s(&["run", "build"]), cwd, &[]).kind, JobKind::RamHeavy);
        assert_eq!(classify("pnpm", &s(&["test"]), cwd, &[]).kind, JobKind::RamHeavy);
        assert_eq!(classify("npm", &s(&["install"]), cwd, &[]).kind, JobKind::Light);
        assert_eq!(classify("npx", &s(&["next", "build"]), cwd, &[]).kind, JobKind::RamHeavy);
        let llm = classify("ollama", &s(&["run", "llama3:70b"]), cwd, &[]);
        assert_eq!(llm.kind, JobKind::Gpu);
        assert!(llm.est_ram_bytes > 40 * GB);
    }

    #[test]
    fn python_script_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("train.py"), "import torch\n").unwrap();
        assert_eq!(classify("python3", &s(&["-u", "train.py"]), dir.path(), &[]).kind, JobKind::Gpu);
    }

    #[test]
    fn opt_in_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".pcbridge"), "# comment\noffload = [\"npm\", 'docker']\n").unwrap();
        let sub = dir.path().join("packages/web");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(project_opt_ins(&sub), vec!["npm", "docker"]);
        assert!(parse_opt_ins("nothing").is_empty());
    }

    #[test]
    fn model_size() {
        assert_eq!(model_billions("llama3:70b"), Some(70.0));
        assert_eq!(model_billions("qwen2.5-14b-instruct"), Some(14.0));
        assert_eq!(model_billions("phi3"), None);
    }

    #[test]
    fn nvenc_rewrite() {
        let r =
            ffmpeg_to_nvenc(&s(&["-i", "a.mov", "-c:v", "libx264", "-crf", "20", "-preset", "slow", "o.mp4"])).unwrap();
        assert_eq!(r, s(&["-i", "a.mov", "-c:v", "h264_nvenc", "-cq", "20", "-preset", "p6", "o.mp4"]));
        let h = ffmpeg_to_nvenc(&s(&["-i", "a", "-vcodec", "libx265", "o"])).unwrap();
        assert!(h.contains(&"hevc_nvenc".to_string()));
        assert!(ffmpeg_to_nvenc(&s(&["-i", "a", "-c:v", "libx264", "-tune", "film", "o"])).is_none());
        assert!(ffmpeg_to_nvenc(&s(&["-i", "a", "-c:v", "prores_ks", "o"])).is_none());
        assert!(ffmpeg_to_nvenc(&s(&["-i", "a", "o"])).is_none());
    }
}
