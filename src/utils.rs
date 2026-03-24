use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

pub enum BashOutput {
    Line(String),
    Done(Value),
}

pub const DEFAULT_TIMEOUT_MS: u64 = 60_000;
pub const OUTPUT_LIMIT: usize = 8_000;

pub fn find_root(start: &Path, markers: &[&str]) -> Option<PathBuf> {
    let mut cur = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };
    loop {
        for marker in markers {
            if cur.join(marker).exists() {
                return Some(cur);
            }
        }
        if !cur.pop() {
            return None;
        }
    }
}

pub fn path_env_with_cargo() -> String {
    #[cfg(windows)]
    let (home_var, cargo_suffix, sep) = ("USERPROFILE", r".cargo\bin", ";");
    #[cfg(not(windows))]
    let (home_var, cargo_suffix, sep) = ("HOME", ".cargo/bin", ":");

    #[cfg(windows)]
    let path_var = "Path";
    #[cfg(not(windows))]
    let path_var = "PATH";

    let current = std::env::var(path_var)
        .or_else(|_| std::env::var("PATH"))
        .unwrap_or_default();
    let cargo_bin = std::env::var(home_var)
        .map(|h| format!("{h}{}{cargo_suffix}", std::path::MAIN_SEPARATOR))
        .unwrap_or_default();
    if cargo_bin.is_empty() || current.contains(&cargo_bin) {
        current
    } else {
        format!("{cargo_bin}{sep}{current}")
    }
}

pub fn truncate_output(s: String) -> Value {
    let total = s.len();
    if total <= OUTPUT_LIMIT {
        return json!({ "output": s, "truncated": false });
    }
    let mut cut = OUTPUT_LIMIT;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    json!({
        "output": &s[..cut],
        "truncated": true,
        "total_bytes": total,
        "shown_bytes": cut,
    })
}

pub fn run_bash_streaming(command: String, timeout_ms: u64) -> BoxStream<'static, BashOutput> {
    async_stream::stream! {
        use tokio::io::{AsyncBufReadExt, BufReader};
        use tokio::time::Duration;

        #[cfg(windows)]
        let (prog, args) = ("cmd", vec!["/C".to_string(), command.clone()]);
        #[cfg(not(windows))]
        let (prog, args) = ("bash", vec!["-c".to_string(), command.clone()]);

        let path_env = path_env_with_cargo();

        let mut child = match Command::new(prog)
            .args(&args)
            .env("PATH", &path_env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                yield BashOutput::Done(json!({ "error": format!("spawn failed: {e}") }));
                return;
            }
        };

        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let mut stdout_lines = BufReader::new(stdout).lines();
        let mut stderr_lines = BufReader::new(stderr).lines();

        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        let mut timed_out = false;
        let mut output_buf = String::new();

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                timed_out = true;
                let _ = child.kill().await;
                break;
            }
            tokio::select! {
                line = stdout_lines.next_line() => {
                    match line {
                        Ok(Some(l)) => {
                            output_buf.push_str(&l);
                            output_buf.push('\n');
                            yield BashOutput::Line(l);
                        }
                        _ => break,
                    }
                }
                line = stderr_lines.next_line() => {
                    if let Ok(Some(l)) = line {
                        output_buf.push_str(&l);
                        output_buf.push('\n');
                        yield BashOutput::Line(format!("[stderr] {l}"));
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    timed_out = true;
                    let _ = child.kill().await;
                    break;
                }
            }
        }

        if timed_out {
            yield BashOutput::Done(json!({ "error": format!("timed out after {timeout_ms}ms"), "timed_out": true }));
            return;
        }

        let exit_code = child.wait().await.ok().and_then(|s| s.code());
        let mut r = truncate_output(output_buf);
        r["exit_code"] = json!(exit_code);
        r["timed_out"] = json!(false);
        yield BashOutput::Done(r);
    }.boxed()
}

pub async fn run_bash(command: &str, timeout_ms: u64) -> Value {
    use tokio::io::AsyncReadExt;
    use tokio::time::{Duration, sleep};

    #[cfg(windows)]
    let (prog, args) = ("cmd", vec!["/C", command]);
    #[cfg(not(windows))]
    let (prog, args) = ("bash", vec!["-c", command]);

    let path_env = path_env_with_cargo();

    let mut child = match Command::new(prog)
        .args(&args)
        .env("PATH", &path_env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return json!({ "error": format!("spawn failed: {e}") }),
    };

    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");

    let read_all = async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        tokio::join!(
            async {
                let _ = stdout.read_to_end(&mut out).await;
            },
            async {
                let _ = stderr.read_to_end(&mut err).await;
            },
        );
        (out, err)
    };

    tokio::select! {
        (stdout_bytes, stderr_bytes) = read_all => {
            let exit_code = child.wait().await.ok().and_then(|s| s.code());
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&stdout_bytes),
                String::from_utf8_lossy(&stderr_bytes),
            );
            let mut r = truncate_output(combined);
            r["exit_code"] = json!(exit_code);
            r["timed_out"] = json!(false);
            r
        }
        _ = sleep(Duration::from_millis(timeout_ms)) => {
            let _ = child.kill().await;
            json!({
                "error": format!("timed out after {timeout_ms}ms"),
                "timed_out": true,
            })
        }
    }
}
