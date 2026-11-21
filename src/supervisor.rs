// Runs child processes, captures their output and keeps track of their state.
//
// Every process gets its own process group on unix so stop/restart takes the
// whole tree down (`sh -c "npm start"` -> node -> whatever node spawned).

use crate::api::Client;
use crate::config::SecretRef;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const LOG_CAP: usize = 5000;
pub const HISTORY: usize = 300; // samples, one a second -> 5 minutes
const STOP_GRACE: Duration = Duration::from_secs(4);
pub const MASK: &str = "••••";

// --- logs ---

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stream {
    Out,
    Err,
    Sys,
}

#[derive(Clone, Debug)]
pub struct LogLine {
    pub time: String,
    pub stream: Stream,
    pub text: String,
}

#[derive(Default)]
pub struct Logs {
    pub lines: VecDeque<LogLine>,
    pub total: u64,
    redact: Vec<String>,
}

pub type SharedLogs = Arc<Mutex<Logs>>;

impl Logs {
    pub fn push(&mut self, stream: Stream, text: &str) {
        let clean = strip_ansi(text);
        let text = redact(&clean, &self.redact);
        if self.lines.len() >= LOG_CAP {
            self.lines.pop_front();
        }
        self.total += 1;
        self.lines.push_back(LogLine { time: chrono::Local::now().format("%H:%M:%S").to_string(), stream, text });
    }

    pub fn sys(&mut self, text: impl AsRef<str>) {
        self.push(Stream::Sys, text.as_ref());
    }

    /// Values to mask in anything printed from now on.
    pub fn set_redact(&mut self, values: &[String]) {
        let mut list = vec![];
        for v in values {
            // multi line secrets (keys, certs) get masked line by line
            for part in std::iter::once(v.as_str()).chain(v.lines()) {
                let p = part.trim();
                // super short values would mask half the log, skip those
                if p.len() >= 4 && !list.iter().any(|x: &String| x == p) {
                    list.push(p.to_string());
                }
            }
        }
        // longest first so a secret that contains another one masks fully
        list.sort_by_key(|s| std::cmp::Reverse(s.len()));
        self.redact = list;
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }
}

pub fn redact(line: &str, secrets: &[String]) -> String {
    let mut out = line.to_string();
    for s in secrets {
        if out.contains(s.as_str()) {
            out = out.replace(s.as_str(), MASK);
        }
    }
    out
}

/// Drop terminal color codes, they just show up as junk in the ui.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            if it.peek() == Some(&'[') {
                it.next();
                // CSI: params until a letter
                for c in it.by_ref() {
                    if c.is_ascii_alphabetic() || c == '~' {
                        break;
                    }
                }
            } else if it.peek() == Some(&']') {
                // OSC: until BEL or ESC \
                for c in it.by_ref() {
                    if c == '\x07' || c == '\\' {
                        break;
                    }
                }
            } else {
                it.next();
            }
        } else if c != '\r' {
            out.push(c);
        }
    }
    out
}

// --- secrets ---

#[derive(Clone, Debug)]
pub struct Injected {
    pub env: String,
    pub vault: String,
    pub vault_id: String,
    pub path: String,
    pub version: i64,
    pub secret_id: String,
}

pub struct Resolved {
    pub vars: Vec<(String, String)>,
    pub injected: Vec<Injected>,
}

/// Fetch every secret a process wants. All or nothing - starting a process
/// with half its config is worse than not starting it.
pub fn resolve(client: Option<&Arc<Mutex<Client>>>, refs: &[SecretRef]) -> Result<Resolved, String> {
    let mut res = Resolved { vars: vec![], injected: vec![] };
    let refs: Vec<_> = refs.iter().filter(|r| !r.env.trim().is_empty()).collect();
    if refs.is_empty() {
        return Ok(res);
    }
    let client = client.ok_or("this needs secrets but you're not connected to 1Claw")?;
    let mut c = client.lock().unwrap();
    let vaults = c.vaults()?;
    for r in refs {
        let v = vaults
            .iter()
            .find(|v| v.name == r.vault || v.id == r.vault)
            .ok_or_else(|| format!("no vault called \"{}\"", r.vault))?;
        let s = c.secret(&v.id, r.path.trim_matches('/')).map_err(|e| format!("{}/{}: {e}", r.vault, r.path))?;
        res.injected.push(Injected {
            env: r.env.trim().to_string(),
            vault: v.name.clone(),
            vault_id: v.id.clone(),
            path: s.path.clone(),
            version: s.version,
            secret_id: s.id.clone(),
        });
        res.vars.push((r.env.trim().to_string(), s.value));
    }
    Ok(res)
}

// --- spawning ---

pub enum Program {
    Shell(String),
    Args(Vec<String>),
}

pub struct Spec {
    pub program: Program,
    pub cwd: String,
    pub env: Vec<(String, String)>,
}

fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~") {
        if let Some(h) = directories::UserDirs::new() {
            return h.home_dir().join(rest.trim_start_matches('/'));
        }
    }
    PathBuf::from(p)
}

pub fn spawn(spec: &Spec, logs: &SharedLogs, ctx: &eframe::egui::Context) -> Result<Child, String> {
    let mut cmd = match &spec.program {
        #[cfg(unix)]
        Program::Shell(s) => {
            let mut c = Command::new("sh");
            c.arg("-c").arg(s);
            c
        }
        #[cfg(windows)]
        Program::Shell(s) => {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(s);
            c
        }
        Program::Args(a) => {
            let (first, rest) = a.split_first().ok_or("empty command")?;
            let mut c = Command::new(first);
            c.args(rest);
            c
        }
    };
    if !spec.cwd.trim().is_empty() {
        let dir = expand_home(spec.cwd.trim());
        if !dir.is_dir() {
            return Err(format!("working dir {} doesn't exist", dir.display()));
        }
        cmd.current_dir(dir);
    }
    // most runtimes buffer stdout when it isn't a tty, which makes the live log useless
    cmd.env("PYTHONUNBUFFERED", "1");
    cmd.envs(spec.env.iter().map(|(k, v)| (k, v)));
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn().map_err(|e| format!("couldn't start: {e}"))?;
    if let Some(out) = child.stdout.take() {
        pipe(out, Stream::Out, logs.clone(), ctx.clone());
    }
    if let Some(err) = child.stderr.take() {
        pipe(err, Stream::Err, logs.clone(), ctx.clone());
    }
    Ok(child)
}

fn pipe(r: impl Read + Send + 'static, stream: Stream, logs: SharedLogs, ctx: eframe::egui::Context) {
    std::thread::spawn(move || {
        let mut r = BufReader::new(r);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match r.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf);
                    logs.lock().unwrap().push(stream, line.trim_end_matches(['\n', '\r']));
                    ctx.request_repaint();
                }
            }
        }
    });
}

pub fn terminate(child: &mut Child, force: bool) {
    #[cfg(unix)]
    {
        let sig = if force { libc::SIGKILL } else { libc::SIGTERM };
        // negative pid = whole process group
        unsafe {
            libc::kill(-(child.id() as i32), sig);
        }
    }
    #[cfg(windows)]
    {
        let mut c = Command::new("taskkill");
        c.args(["/PID", &child.id().to_string(), "/T"]);
        if force {
            c.arg("/F");
        }
        if c.status().map(|s| !s.success()).unwrap_or(true) || force {
            let _ = child.kill();
        }
    }
}

// --- runtime state ---

#[derive(Clone, PartialEq, Debug)]
pub enum Status {
    Stopped,
    Starting,
    Running,
    Stopping,
    Exited(i32),
    Crashed(i32),
    Failed(String),
    Backoff,
}

impl Status {
    pub fn is_live(&self) -> bool {
        matches!(self, Status::Starting | Status::Running | Status::Stopping | Status::Backoff)
    }
}

pub enum Tick {
    Nothing,
    Start,
    Exited,
}

pub struct Runtime {
    pub status: Status,
    pub child: Option<Child>,
    pub started_at: Option<Instant>,
    pub ended_at: Option<Instant>,
    pub restarts: u32,
    pub logs: SharedLogs,
    pub injected: Vec<Injected>,
    pub cpu: VecDeque<[f64; 2]>,
    pub mem: VecDeque<[f64; 2]>,
    pub temp_file: Option<PathBuf>,
    /// bumped on every start/stop so late results from a cancelled start get ignored
    pub generation: u64,
    /// how many times it actually got going (with its secrets loaded)
    pub runs: u64,
    stop_requested: Option<Instant>,
    force_sent: bool,
    restart_after_stop: bool,
    next_start: Option<Instant>,
    crashes: VecDeque<Instant>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            status: Status::Stopped,
            child: None,
            started_at: None,
            ended_at: None,
            restarts: 0,
            logs: Arc::new(Mutex::new(Logs::default())),
            injected: vec![],
            cpu: VecDeque::new(),
            mem: VecDeque::new(),
            temp_file: None,
            generation: 0,
            runs: 0,
            stop_requested: None,
            force_sent: false,
            restart_after_stop: false,
            next_start: None,
            crashes: VecDeque::new(),
        }
    }
}

impl Runtime {
    pub fn log(&self, s: impl AsRef<str>) {
        self.logs.lock().unwrap().sys(s);
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(|c| c.id())
    }

    pub fn uptime(&self) -> Option<Duration> {
        match self.status {
            Status::Running | Status::Stopping => self.started_at.map(|t| t.elapsed()),
            _ => None,
        }
    }

    /// Marks the start as in flight. Returns the generation the worker should report back with.
    pub fn begin_start(&mut self) -> u64 {
        self.generation += 1;
        self.status = Status::Starting;
        self.next_start = None;
        self.stop_requested = None;
        self.restart_after_stop = false;
        self.generation
    }

    pub fn started(&mut self, child: Child, injected: Vec<Injected>) {
        self.log(format!("started, pid {}", child.id()));
        self.child = Some(child);
        self.injected = injected;
        self.runs += 1;
        self.started_at = Some(Instant::now());
        self.status = Status::Running;
        self.cpu.clear();
        self.mem.clear();
    }

    pub fn failed(&mut self, e: String) {
        self.log(format!("failed to start: {e}"));
        self.status = Status::Failed(e);
    }

    pub fn stop(&mut self, restart: bool) {
        self.restart_after_stop = restart;
        self.next_start = None;
        match (&mut self.child, &self.status) {
            (Some(c), _) => {
                if self.stop_requested.is_none() {
                    self.logs.lock().unwrap().sys(if restart { "restarting..." } else { "stopping..." });
                    terminate(c, false);
                    self.stop_requested = Some(Instant::now());
                    self.force_sent = false;
                }
                self.status = Status::Stopping;
            }
            (None, Status::Starting | Status::Backoff) => {
                self.generation += 1; // cancel whatever is in flight
                self.status = Status::Stopped;
                self.log("cancelled");
            }
            _ => {}
        }
    }

    /// Call often (every frame is fine). Handles exits, force kills and crash backoff.
    pub fn tick(&mut self, autorestart: bool) -> Tick {
        if let Some(t) = self.stop_requested {
            if !self.force_sent && t.elapsed() > STOP_GRACE {
                if let Some(c) = self.child.as_mut() {
                    self.logs.lock().unwrap().sys("didn't stop in time, killing it");
                    terminate(c, true);
                }
                self.force_sent = true;
            }
        }

        if let Some(c) = self.child.as_mut() {
            let Ok(Some(st)) = c.try_wait() else { return Tick::Nothing };
            let code = exit_code(st);
            self.child = None;
            self.ended_at = Some(Instant::now());
            if let Some(f) = self.temp_file.take() {
                let _ = std::fs::remove_file(f);
            }

            if self.stop_requested.take().is_some() {
                self.status = Status::Stopped;
                self.log("stopped");
                if std::mem::take(&mut self.restart_after_stop) {
                    self.restarts += 1;
                    return Tick::Start;
                }
            } else if code == 0 {
                self.status = Status::Exited(0);
                self.log("exited cleanly");
            } else {
                self.status = Status::Crashed(code);
                self.log(format!("exited with code {code}"));
                if autorestart {
                    self.schedule_restart();
                }
            }
            return Tick::Exited;
        }

        if let Some(t) = self.next_start {
            if Instant::now() >= t {
                self.next_start = None;
                self.restarts += 1;
                return Tick::Start;
            }
        }
        Tick::Nothing
    }

    fn schedule_restart(&mut self) {
        let now = Instant::now();
        self.crashes.push_back(now);
        while self.crashes.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60)) {
            self.crashes.pop_front();
        }
        let n = self.crashes.len() as u32;
        if n >= 8 {
            self.log("crashed 8 times in a minute, giving up. fix it and hit start");
            return;
        }
        // 1s, 2s, 4s ... capped at 30s
        let wait = Duration::from_secs((1u64 << (n - 1).min(5)).min(30));
        self.log(format!("restarting in {}s", wait.as_secs()));
        self.next_start = Some(now + wait);
        self.status = Status::Backoff;
    }

    pub fn push_sample(&mut self, t: f64, cpu: f32, mem_bytes: u64) {
        for (q, v) in [(&mut self.cpu, cpu as f64), (&mut self.mem, mem_bytes as f64 / 1_048_576.0)] {
            if q.len() >= HISTORY {
                q.pop_front();
            }
            q.push_back([t, v]);
        }
    }
}

fn exit_code(st: std::process::ExitStatus) -> i32 {
    if let Some(c) = st.code() {
        return c;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = st.signal() {
            return 128 + sig; // same convention as shells
        }
    }
    -1
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // never leave orphans behind when the app closes
        if let Some(c) = self.child.as_mut() {
            terminate(c, true);
            let _ = c.wait();
        }
        if let Some(f) = self.temp_file.take() {
            let _ = std::fs::remove_file(f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_secrets() {
        let mut l = Logs::default();
        l.set_redact(&["sk_live_abc123".into(), "ab".into()]);
        l.push(Stream::Out, "key=sk_live_abc123 ab");
        assert_eq!(l.lines[0].text, format!("key={MASK} ab"));
    }

    #[test]
    fn masks_multiline_secret_lines() {
        let mut l = Logs::default();
        l.set_redact(&["-----BEGIN KEY-----\nMIIEvQIBADANBgkq\n-----END KEY-----".into()]);
        l.push(Stream::Out, "oops MIIEvQIBADANBgkq leaked");
        assert_eq!(l.lines[0].text, format!("oops {MASK} leaked"));
    }

    #[test]
    fn strips_colors() {
        assert_eq!(strip_ansi("\x1b[32mok\x1b[0m done\r"), "ok done");
    }

    #[test]
    fn caps_log() {
        let mut l = Logs::default();
        for i in 0..LOG_CAP + 10 {
            l.push(Stream::Out, &i.to_string());
        }
        assert_eq!(l.lines.len(), LOG_CAP);
        assert_eq!(l.lines[0].text, "10");
    }

    #[cfg(unix)]
    #[test]
    fn runs_and_reports_exit() {
        let ctx = eframe::egui::Context::default();
        let mut rt = Runtime::default();
        let spec = Spec { program: Program::Shell("echo hi; exit 3".into()), cwd: String::new(), env: vec![] };
        let child = spawn(&spec, &rt.logs, &ctx).unwrap();
        rt.begin_start();
        rt.started(child, vec![]);
        let t = Instant::now();
        while rt.child.is_some() && t.elapsed() < Duration::from_secs(5) {
            rt.tick(false);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(rt.status, Status::Crashed(3));
        std::thread::sleep(Duration::from_millis(50));
        assert!(rt.logs.lock().unwrap().lines.iter().any(|l| l.text == "hi"));
    }

    #[cfg(unix)]
    #[test]
    fn stop_kills_whole_group() {
        let ctx = eframe::egui::Context::default();
        let mut rt = Runtime::default();
        let spec = Spec { program: Program::Shell("sleep 30 & sleep 30".into()), cwd: String::new(), env: vec![] };
        let child = spawn(&spec, &rt.logs, &ctx).unwrap();
        rt.begin_start();
        rt.started(child, vec![]);
        rt.stop(false);
        let t = Instant::now();
        while rt.child.is_some() && t.elapsed() < Duration::from_secs(6) {
            rt.tick(false);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(rt.status, Status::Stopped);
    }
}
