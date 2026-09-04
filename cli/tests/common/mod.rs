use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

pub const BIN: &str = env!("CARGO_BIN_EXE_mev-scout");

pub static RPC_MUTEX: Mutex<()> = Mutex::new(());

pub const TEST_TIMEOUT: Duration = Duration::from_secs(120);
pub const NETWORK_TIMEOUT: Duration = Duration::from_secs(300);
pub const HEAVY_TIMEOUT: Duration = Duration::from_secs(600);

pub struct TimedOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl TimedOutput {
    pub fn combined(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

pub fn repo_config() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("mev-scout.toml")
}

pub fn first_rpc_url() -> Option<String> {
    let text = fs::read_to_string(repo_config()).ok()?;
    let start = text.find("https://")?;
    let end = text[start..]
        .find(|c: char| c.is_whitespace() || c == '"' || c == ',' || c == ']')
        .map(|i| start + i)
        .unwrap_or(text.len());
    Some(text[start..end].to_string())
}

pub fn temp_ws(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("mev_scout_e2e_{tag}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    fs::create_dir_all(p.join("cache")).unwrap();
    p
}

/// Clone the repo `mev-scout.toml` into `ws`, replacing or appending the given
/// flat `key = value` lines. Handles the multi-line `rpc_urls` array replacement.
/// Values are auto-formatted: quoted strings keep their quotes (backslashes
/// escaped), bare strings get quoted with backslashes escaped, numbers/bools/
/// arrays pass through verbatim.
pub fn temp_config(ws: &Path, extras: &[(&str, &str)]) -> PathBuf {
    let src = fs::read_to_string(repo_config()).unwrap_or_default();
    let mut lines: Vec<String> = src.lines().map(String::from).collect();
    for (key, value) in extras {
        let needle = format!("{key} =");
        let value = fmt_toml_value(value);
        let mut replaced = false;
        for i in 0..lines.len() {
            if lines[i].trim_start().starts_with(&needle) {
                lines[i] = format!("{key} = {value}");
                if lines[i].contains('[') && !lines[i].contains(']') {
                    let mut j = i + 1;
                    while j < lines.len() && !lines[j].contains(']') {
                        j += 1;
                    }
                    if j < lines.len() {
                        lines.drain(i + 1..=j);
                    }
                }
                replaced = true;
                break;
            }
        }
        if !replaced {
            lines.push(format!("{key} = {value}"));
        }
    }
    let out = ws.join("mev-scout.toml");
    fs::write(&out, lines.join("\n")).unwrap();
    out
}

fn fmt_toml_value(value: &str) -> String {
    if value.starts_with('"') && value.ends_with('"') {
        value.replace('\\', "\\\\")
    } else if value.starts_with('[')
        || value.parse::<f64>().is_ok()
        || value == "true"
        || value == "false"
    {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('\\', "\\\\"))
    }
}

pub fn scout(cwd: &Path) -> Command {
    let mut c = Command::new(BIN);
    c.current_dir(cwd);
    c
}

fn read_file(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_default()
}

pub fn run_timed(cmd: &mut Command, timeout: Duration) -> Result<TimedOutput, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let out_path = std::env::temp_dir().join(format!("mscout_out_{stamp}.txt"));
    let err_path = std::env::temp_dir().join(format!("mscout_err_{stamp}.txt"));

    cmd.stdout(Stdio::from(
        fs::File::create(&out_path).map_err(|e| e.to_string())?,
    ));
    cmd.stderr(Stdio::from(
        fs::File::create(&err_path).map_err(|e| e.to_string())?,
    ));
    cmd.stdin(Stdio::null());

    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawn {program} failed: {e}"))?;

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = read_file(&out_path);
                let stderr = read_file(&err_path);
                let _ = fs::remove_file(&out_path);
                let _ = fs::remove_file(&err_path);
                return Ok(TimedOutput {
                    success: status.success(),
                    code: status.code(),
                    stdout,
                    stderr,
                });
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let stdout = read_file(&out_path);
                    let stderr = read_file(&err_path);
                    let _ = fs::remove_file(&out_path);
                    let _ = fs::remove_file(&err_path);
                    return Err(format!(
                        "TIMEOUT after {timeout:?}: {program}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
                    ));
                }
                thread::sleep(Duration::from_millis(150));
            }
            Err(e) => return Err(format!("wait failed: {e}")),
        }
    }
}

pub fn expect_ok(out: &TimedOutput, ctx: &str) {
    assert!(
        out.success,
        "{ctx} failed (exit {:?})\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.code, out.stdout, out.stderr
    );
}

pub fn expect_fail(out: &TimedOutput, ctx: &str) {
    assert!(
        !out.success,
        "{ctx} unexpectedly succeeded\n--- stdout ---\n{}",
        out.stdout
    );
}

pub fn rpc_ready(ws: &Path) -> bool {
    let mut c = scout(ws);
    c.args([
        "-f",
        repo_config().to_str().unwrap(),
        "scan",
        "--kind",
        "trades",
        "--blocks",
        "1",
        "--limit",
        "1",
    ]);
    matches!(run_timed(&mut c, NETWORK_TIMEOUT), Ok(o) if o.success)
}

pub fn ensure_gate_and_rpc(tag: &str) -> Option<PathBuf> {
    if std::env::var("MEV_SCOUT_E2E").as_deref() != Ok("1") {
        eprintln!("SKIP: set MEV_SCOUT_E2E=1 to run live-Polygon E2E tests");
        return None;
    }
    let ws = temp_ws(tag);
    if !rpc_ready(&ws) {
        eprintln!("SKIP: Polygon RPCs from mev-scout.toml unreachable");
        return None;
    }
    Some(ws)
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn extract_json_array(s: &str) -> Option<serde_json::Value> {
    let clean = strip_ansi(s);
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(clean.trim()) {
        return Some(v);
    }
    let end = clean.rfind(']')?;
    for (idx, _) in clean.match_indices('[') {
        if let Ok(v) = serde_json::from_str(&clean[idx..=end]) {
            return Some(v);
        }
    }
    None
}
