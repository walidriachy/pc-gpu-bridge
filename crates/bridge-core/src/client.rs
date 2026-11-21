//! Blocking HTTP client for the PC agent (used by the app, the shims and the MCP server).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("can't reach the PC: {0}")]
    Unreachable(String),
    #[error("the PC says you are not paired (wrong or missing token)")]
    Unauthorized,
    #[error("the PC refused: {0}")]
    Refused(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, ClientError>;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunRequest {
    pub argv: Vec<String>,
    pub cwd: String,
    pub name: Option<String>,
    pub gpu: bool,
    pub inputs: Vec<String>,
    pub cache: bool,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct JobOutcome {
    pub job_id: Option<String>,
    pub exit_code: i32,
    pub outputs: Vec<String>,
    pub cached: bool,
    pub duration: f64,
    pub folder: Option<String>,
    pub result_text: Option<String>,
}

#[derive(Clone)]
pub struct AgentClient {
    pub base: String,
    token: Option<String>,
    http: reqwest::blocking::Client,
}

fn map_err(e: reqwest::Error) -> ClientError {
    if e.is_connect() || e.is_timeout() {
        ClientError::Unreachable(e.to_string())
    } else {
        ClientError::Other(e.to_string())
    }
}

impl AgentClient {
    pub fn new(base: impl Into<String>, token: Option<String>) -> Self {
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(4))
            .timeout(None)
            .build()
            .expect("http client");
        Self { base: base.into().trim_end_matches('/').to_string(), token, http }
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
        let mut r = self.http.request(method, format!("{}{}", self.base, path));
        if let Some(t) = &self.token {
            r = r.bearer_auth(t);
        }
        r
    }

    fn check(resp: reqwest::blocking::Response) -> Result<reqwest::blocking::Response> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body: Value = resp.json().unwrap_or(Value::Null);
        let detail = body.get("detail").map(|d| d.to_string().trim_matches('"').to_string()).unwrap_or_default();
        match status.as_u16() {
            401 => Err(ClientError::Unauthorized),
            403 | 404 | 422 => Err(ClientError::Refused(detail)),
            _ => Err(ClientError::Other(format!("HTTP {status}: {detail}"))),
        }
    }

    fn get_json(&self, path: &str, timeout: u64) -> Result<Value> {
        let r = self.req(reqwest::Method::GET, path).timeout(Duration::from_secs(timeout)).send().map_err(map_err)?;
        Self::check(r)?.json().map_err(|e| ClientError::Other(e.to_string()))
    }

    pub fn health(&self) -> Result<Value> {
        self.get_json("/health", 4)
    }
    pub fn status(&self) -> Result<Value> {
        self.get_json("/status", 8)
    }
    pub fn gpu(&self) -> Result<Value> {
        self.get_json("/gpu", 15)
    }
    pub fn memory(&self) -> Result<Value> {
        self.get_json("/memory", 8)
    }
    pub fn jobs(&self) -> Result<Value> {
        self.get_json("/jobs", 8)
    }

    pub fn pair(&self, code: &str, device_name: &str) -> Result<Value> {
        let r = self
            .req(reqwest::Method::POST, "/pair")
            .timeout(Duration::from_secs(10))
            .json(&json!({"code": code, "device_name": device_name}))
            .send()
            .map_err(map_err)?;
        Self::check(r)?.json().map_err(|e| ClientError::Other(e.to_string()))
    }

    pub fn cancel(&self, job_id: &str) -> Result<()> {
        let r = self.req(reqwest::Method::POST, &format!("/jobs/{job_id}/cancel")).send().map_err(map_err)?;
        Self::check(r).map(|_| ())
    }

    /// Stream NDJSON events from a POST; `on_event` sees every event as it arrives.
    fn stream(&self, path: &str, body: &Value, mut on_event: impl FnMut(&Value)) -> Result<JobOutcome> {
        let r = self.req(reqwest::Method::POST, path).json(body).send().map_err(map_err)?;
        let r = Self::check(r)?;
        let mut out = JobOutcome { exit_code: -1, ..Default::default() };
        for line in BufReader::new(r).lines() {
            let line = line.map_err(|e| ClientError::Unreachable(format!("connection dropped: {e}")))?;
            if line.trim().is_empty() {
                continue;
            }
            let ev: Value = serde_json::from_str(&line).map_err(|e| ClientError::Other(e.to_string()))?;
            match ev["type"].as_str().unwrap_or("") {
                "job" => {
                    out.job_id = ev["id"].as_str().map(String::from);
                    out.folder = ev["folder"].as_str().map(String::from);
                }
                "cached" => out.cached = true,
                "claude" => {
                    if ev["event"]["type"] == "result" {
                        out.result_text = ev["event"]["result"].as_str().map(String::from);
                    }
                }
                "exit" => {
                    out.exit_code = ev["code"].as_i64().unwrap_or(-1) as i32;
                    out.duration = ev["duration"].as_f64().unwrap_or(0.0);
                    out.outputs = ev["outputs"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                        .unwrap_or_default();
                }
                _ => {}
            }
            on_event(&ev);
        }
        if out.exit_code == -1 && out.job_id.is_some() {
            return Err(ClientError::Unreachable("the connection to the PC ended before the job finished".into()));
        }
        Ok(out)
    }

    pub fn run(&self, req: &RunRequest, on_event: impl FnMut(&Value)) -> Result<JobOutcome> {
        let mut body = serde_json::to_value(req).unwrap();
        body["stream"] = json!(true);
        self.stream("/run", &body, on_event)
    }

    pub fn delegate(&self, task: &str, files: &[String], on_event: impl FnMut(&Value)) -> Result<JobOutcome> {
        self.stream("/delegate", &json!({"task": task, "files": files, "stream": true}), on_event)
    }

    pub fn stat(&self, rel: &str) -> Result<Value> {
        let r = self.req(reqwest::Method::GET, "/files/stat").query(&[("path", rel)]).send().map_err(map_err)?;
        Self::check(r)?.json().map_err(|e| ClientError::Other(e.to_string()))
    }

    pub fn push_file(&self, local: &Path, rel: &str) -> Result<Value> {
        let form = reqwest::blocking::multipart::Form::new()
            .text("path", rel.to_string())
            .file("file", local)
            .map_err(|e| ClientError::Other(e.to_string()))?;
        let r = self.req(reqwest::Method::POST, "/files/push").multipart(form).send().map_err(map_err)?;
        Self::check(r)?.json().map_err(|e| ClientError::Other(e.to_string()))
    }

    pub fn pull_file(&self, rel: &str, local: &Path) -> Result<u64> {
        let r = self.req(reqwest::Method::GET, "/files/pull").query(&[("path", rel)]).send().map_err(map_err)?;
        let mut r = Self::check(r)?;
        if let Some(d) = local.parent() {
            std::fs::create_dir_all(d).map_err(|e| ClientError::Other(e.to_string()))?;
        }
        let tmp = local.with_extension("pcbridge-part");
        let mut f = std::fs::File::create(&tmp).map_err(|e| ClientError::Other(e.to_string()))?;
        let n = r.copy_to(&mut f).map_err(map_err)?;
        std::fs::rename(&tmp, local).map_err(|e| ClientError::Other(e.to_string()))?;
        Ok(n)
    }

    pub fn ollama_chat(&self, model: &str, messages: Value) -> Result<Value> {
        let r = self
            .req(reqwest::Method::POST, "/ollama/chat")
            .json(&json!({"model": model, "messages": messages}))
            .send()
            .map_err(map_err)?;
        Self::check(r)?.json().map_err(|e| ClientError::Other(e.to_string()))
    }

    pub fn comfy_generate(&self, workflow: Value) -> Result<Value> {
        let r = self
            .req(reqwest::Method::POST, "/comfy/generate")
            .json(&json!({"workflow": workflow}))
            .send()
            .map_err(map_err)?;
        Self::check(r)?.json().map_err(|e| ClientError::Other(e.to_string()))
    }

    /// Measure throughput in megabits/s (upload + download of `mb` megabytes).
    pub fn speedtest(&self, mb: usize) -> Result<(f64, f64)> {
        let data = vec![0u8; mb * 1_048_576];
        let t = std::time::Instant::now();
        let part = reqwest::blocking::multipart::Part::bytes(data).file_name("speed.bin");
        let form = reqwest::blocking::multipart::Form::new().part("file", part);
        let r = self.req(reqwest::Method::POST, "/files/speedtest").multipart(form).send().map_err(map_err)?;
        Self::check(r)?;
        let up = (mb as f64 * 8.0) / t.elapsed().as_secs_f64();
        let t = std::time::Instant::now();
        let r = self.req(reqwest::Method::GET, &format!("/files/speedtest?mb={mb}")).send().map_err(map_err)?;
        let bytes = Self::check(r)?.bytes().map_err(map_err)?;
        let down = (bytes.len() as f64 / 1_048_576.0 * 8.0) / t.elapsed().as_secs_f64();
        Ok((up, down))
    }
}
