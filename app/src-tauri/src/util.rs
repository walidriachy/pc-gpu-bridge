//! Small helpers: run programs without flashing console windows, and run blocking work off the UI thread.

use std::process::Command;

pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

pub fn command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

pub fn run(program: &str, args: &[&str]) -> Out {
    match command(program).args(args).output() {
        Ok(o) => Out {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into(),
            stderr: String::from_utf8_lossy(&o.stderr).into(),
        },
        Err(e) => Out { code: -1, stdout: String::new(), stderr: e.to_string() },
    }
}

pub fn powershell(script: &str) -> Out {
    run("powershell", &["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", script])
}

/// Run blocking work (HTTP, installers) on a worker thread so the window stays responsive.
pub async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

pub fn which(program: &str) -> Option<String> {
    let finder = if cfg!(windows) { "where" } else { "which" };
    let o = run(finder, &[program]);
    o.ok().then(|| o.stdout.lines().next().unwrap_or("").trim().to_string()).filter(|s| !s.is_empty())
}

pub fn random_key(len: usize) -> String {
    use rand::Rng;
    const CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let mut r = rand::thread_rng();
    (0..len).map(|_| CHARS[r.gen_range(0..CHARS.len())] as char).collect()
}
