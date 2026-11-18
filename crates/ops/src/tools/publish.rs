//! `publish`: building instructions and studio renders, as background jobs.
//!
//! A job exports the working model to LDraw, runs `sb MODEL --generate-instructions`
//! (optionally `--studio-render`) in a child process, and when it finishes stores
//! every page and image in the project as assets (`project assets` lists them,
//! `project export` writes them out).

use crate::{Reply, Session};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "kind": {"type": "string", "enum": ["instructions", "studio"],
                     "description": "instructions: step pages and PDF. studio: a path-traced beauty render."},
            "job": {"type": "string", "description": "Check (and collect) a job started earlier."},
            "size": {"type": "integer", "description": "Image size in pixels (128–1600, default 768)."},
            "high_quality": {"type": "boolean"},
            "plan_only": {"type": "boolean", "description": "instructions: step plan only, no images."},
            "wait": {"type": "integer", "description": "Seconds to wait for the job before returning (0–600, default 0)."}
        }
    })
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The `sb` binary to run jobs with: `$SB_BIN`, this binary if it is `sb`, else `sb` on PATH.
fn sb_binary() -> PathBuf {
    if let Some(p) = ldraw::app_env("BIN") {
        return PathBuf::from(p);
    }
    if let Ok(me) = std::env::current_exe() {
        if me.file_stem().is_some_and(|n| n == "sb") {
            return me;
        }
    }
    PathBuf::from(ldraw::platform::exe_name("sb"))
}

fn jobs_dir(s: &Session) -> PathBuf {
    let tag = ldraw::num::fnv1a64(s.store.path().to_string_lossy().as_bytes());
    std::env::temp_dir().join("superbrick-jobs").join(format!("{tag:016x}"))
}

/// Jobs this process started. A finished child stays a zombie (and `kill -0` still succeeds)
/// until it is reaped, so a long-lived MCP server must reap its own children to see them end.
static CHILDREN: std::sync::Mutex<Vec<std::process::Child>> = std::sync::Mutex::new(Vec::new());

fn alive(pid: u64) -> bool {
    if let Ok(mut kids) = CHILDREN.lock() {
        if let Some(k) = kids.iter().position(|c| c.id() as u64 == pid) {
            return match kids[k].try_wait() {
                Ok(None) => true,
                _ => {
                    kids.remove(k);
                    false
                }
            };
        }
    }
    u32::try_from(pid).is_ok_and(ldraw::platform::pid_alive)
}

pub fn run(s: &mut Session, args: &Value) -> Result<Reply, String> {
    if let Some(job) = args.get("job").and_then(Value::as_str) {
        return check(s, job, wait_arg(args));
    }
    let Some(kind) = args.get("kind").and_then(Value::as_str) else {
        return list(s);
    };
    if !matches!(kind, "instructions" | "studio") {
        return Err(format!("unknown kind {kind:?}; use instructions or studio"));
    }
    let size = args.get("size").and_then(Value::as_u64).unwrap_or(768);
    if !(128..=1600).contains(&size) {
        return Err("size must be 128–1600".into());
    }
    let tree = s.store.working(&s.branch)?.tree;
    let dir = jobs_dir(s);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let id = format!("j{}", now());
    let mut id_n = id.clone();
    let mut k = 1;
    while dir.join(format!("{id_n}.ldr")).exists() {
        k += 1;
        id_n = format!("{id}-{k}");
    }
    let id = id_n;
    let ldr = dir.join(format!("{id}.ldr"));
    // An agent publishes what it sees: the model with its pending drafts.
    if s.view().parts.is_empty() {
        return Err("the model has no parts yet (nothing accepted or drafted)".into());
    }
    std::fs::write(&ldr, s.export()?).map_err(|e| e.to_string())?;
    let out = dir.join(&id);
    let log = dir.join(format!("{id}.log"));
    let log_file = std::fs::File::create(&log).map_err(|e| e.to_string())?;
    let mut cmd = Command::new(sb_binary());
    cmd.arg(&ldr)
        .arg("--generate-instructions")
        .arg("--output")
        .arg(&out)
        .arg("--image-size")
        .arg(size.to_string());
    if kind == "studio" {
        cmd.arg("--studio-render");
    }
    if args.get("high_quality").and_then(Value::as_bool) == Some(true) {
        cmd.arg("--high-quality");
    }
    if args.get("plan_only").and_then(Value::as_bool) == Some(true) {
        cmd.arg("--plan-only");
    }
    ldraw::platform::no_window(&mut cmd);
    let child = cmd
        .stdin(Stdio::null())
        .stdout(log_file.try_clone().map_err(|e| e.to_string())?)
        .stderr(log_file)
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", sb_binary().display()))?;
    let record = json!({
        "id": id, "kind": kind, "user": s.user, "tree": tree, "pid": child.id(),
        "out": out, "log": log, "started": now(), "state": "running",
    });
    if let Ok(mut kids) = CHILDREN.lock() {
        kids.push(child);
    }
    s.store.set_pref("project", &format!("job:{id}"), &record)?;
    check(s, &id, wait_arg(args))
}

fn wait_arg(args: &Value) -> u64 {
    args.get("wait").and_then(Value::as_u64).unwrap_or(0).min(600)
}

fn list(s: &Session) -> Result<Reply, String> {
    let jobs: Vec<Value> = s
        .store
        .prefs("project")?
        .into_iter()
        .filter(|(k, _)| k.starts_with("job:"))
        .map(|(_, v)| v)
        .collect();
    let mut text = String::from("Publishing jobs:\n");
    for j in &jobs {
        text += &format!(
            "  {} {} — {}\n",
            j["id"].as_str().unwrap_or("?"),
            j["kind"].as_str().unwrap_or("?"),
            j["state"].as_str().unwrap_or("?")
        );
    }
    if jobs.is_empty() {
        text = "No publishing jobs. Start one with {\"kind\":\"instructions\"} or {\"kind\":\"studio\"}.".into();
    }
    Ok(Reply::ok(text, json!({ "jobs": jobs })))
}

fn tail(path: &Path, lines: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// The generator's final `{"output":…,"result":…}` line, when it has finished.
fn result_line(log: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(log).ok()?;
    text.lines()
        .rev()
        .filter(|l| l.starts_with('{'))
        .find_map(|l| serde_json::from_str::<Value>(l).ok().filter(|v| v.get("output").is_some()))
}

fn check(s: &mut Session, id: &str, wait: u64) -> Result<Reply, String> {
    let key = format!("job:{id}");
    let mut job = s.store.pref("project", &key)?.ok_or_else(|| format!("no job {id}"))?;
    let log = PathBuf::from(job["log"].as_str().unwrap_or_default());
    let pid = job["pid"].as_u64().unwrap_or(0);
    let deadline = Instant::now() + Duration::from_secs(wait);
    if job["state"] == "running" {
        loop {
            if let Some(result) = result_line(&log) {
                collect(s, &mut job, &result)?;
                break;
            }
            if !alive(pid) {
                // One last look: the line may have landed just before exit.
                if let Some(result) = result_line(&log) {
                    collect(s, &mut job, &result)?;
                } else {
                    job["state"] = json!("failed");
                    job["error"] = json!(tail(&log, 12));
                }
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        s.store.set_pref("project", &key, &job)?;
    }
    let state = job["state"].as_str().unwrap_or("?").to_string();
    let mut reply = match state.as_str() {
        "running" => Reply::ok(
            format!(
                "Job {id} ({}) is running ({} s so far). Check again with {{\"job\":\"{id}\",\"wait\":60}}.\nLog tail:\n{}",
                job["kind"].as_str().unwrap_or("?"),
                now().saturating_sub(job["started"].as_u64().unwrap_or(0)),
                tail(&log, 4)
            ),
            job.clone(),
        ),
        "done" => Reply::ok(
            format!(
                "Job {id} finished: {} assets stored ({}). Write them out with project {{\"action\":\"export\",\"assets\":\"{id}\",\"path\":DIR}}.",
                job["assets"].as_array().map_or(0, Vec::len),
                job["summary"].as_str().unwrap_or("")
            ),
            job.clone(),
        ),
        _ => Reply::fail(
            format!("Job {id} failed:\n{}", job["error"].as_str().unwrap_or("")),
            job.clone(),
        ),
    };
    if state == "done" {
        // Show the cover image (or first page) so the result can be judged at once.
        if let Some(first) = job["preview"].as_str() {
            if let Ok(asset) = s.store.asset(first) {
                let (w, h) = png_size(&asset.1).unwrap_or((0, 0));
                reply.images.push(crate::Image {
                    png: asset.1,
                    caption: format!("{id}: {}", job["preview_name"].as_str().unwrap_or("preview")),
                    width: w,
                    height: h,
                });
            }
        }
    }
    Ok(reply)
}

fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || &png[1..4] != b"PNG" {
        return None;
    }
    let w = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(png[20..24].try_into().ok()?);
    Some((w, h))
}

fn mime(path: &Path) -> Option<&'static str> {
    Some(match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "pdf" => "application/pdf",
        "html" => "text/html",
        "json" => "application/json",
        "ldr" | "mpd" => "text/plain",
        "txt" | "md" => "text/plain",
        _ => return None,
    })
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Store a finished job's files as assets.
fn collect(s: &mut Session, job: &mut Value, result: &Value) -> Result<(), String> {
    let out = PathBuf::from(result["output"].as_str().or(job["out"].as_str()).unwrap_or_default());
    let kind = job["kind"].as_str().unwrap_or("instructions").to_string();
    let tree = job["tree"].as_str().map(str::to_string);
    let id = job["id"].as_str().unwrap_or_default().to_string();
    let mut files = Vec::new();
    walk(&out, &mut files);
    let mut stored = Vec::new();
    let mut preview: Option<(String, String)> = None;
    for path in files.iter().take(400) {
        let Some(mime) = mime(path) else { continue };
        if path.file_name().is_some_and(|n| n == "job.json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else { continue };
        if bytes.len() > 64 << 20 {
            continue;
        }
        let rel = path.strip_prefix(&out).unwrap_or(path).to_string_lossy().to_string();
        let name = format!("{id}/{rel}");
        let asset = s.store.put_asset(&kind, &name, mime, &bytes, tree.as_deref(), &json!({"job": id}))?;
        if mime == "image/png" {
            let better = |n: &str| n.contains("cover") || n.contains("studio") || n.contains("final");
            if preview.is_none() || (better(&rel) && !preview.as_ref().is_some_and(|p| better(&p.1))) {
                preview = Some((asset.clone(), rel.clone()));
            }
        }
        stored.push(json!({"id": asset, "name": name, "mime": mime, "bytes": bytes.len()}));
    }
    job["state"] = json!("done");
    job["assets"] = json!(stored);
    job["result"] = result["result"].clone();
    job["summary"] = json!(match &result["result"] {
        Value::Object(m) => m
            .iter()
            .filter(|(_, v)| v.is_number() || v.is_string())
            .take(6)
            .map(|(k, v)| format!("{k}={}", v.to_string().trim_matches('"')))
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    });
    if let Some((asset, rel)) = preview {
        job["preview"] = json!(asset);
        job["preview_name"] = json!(rel);
    }
    job["finished"] = json!(now());
    Ok(())
}
