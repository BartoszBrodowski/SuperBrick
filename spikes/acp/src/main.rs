//! S1 spike: an ACP client for `opencode acp`, plus the MCP path the CAD will use.
//!
//!   acp-spike run --model openai/gpt-6-luna [--mcp-via-config] [--only 1,3]
//!   acp-spike mcp-stdio --socket PATH        (the shim opencode launches; relays stdio <-> socket)
//!
//! `run` starts a fake CAD app (a Unix socket, mode 0600, speaking MCP with tools summary/add/look),
//! spawns `opencode acp` in an empty scratch folder with a generated sandbox config (edit, write,
//! bash, webfetch denied), passes the shim in `session/new` `mcpServers` (or, with
//! `--mcp-via-config`, in the generated config instead), and runs four probes:
//!   1 mcp      the agent calls the CAD's `add` and `summary` tools
//!   2 image    an image in the prompt reaches the model (it must name a blue circle)
//!   3 tool-img an image in a tool result reaches the model (it must name a green triangle)
//!   4 sandbox  asked to write a file, it can't (nothing appears in the scratch folder)
//! Everything sent and received is logged to out/<tag>/acp.jsonl; a summary goes to stdout and
//! out/<tag>/report.json.

use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::{UnixListener, UnixStream}},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use base64::Engine;
use serde_json::{Value, json};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    match args.first().map(String::as_str) {
        Some("mcp-stdio") => shim(&get("--socket").expect("--socket PATH")),
        Some("run") => {
            let model = get("--model").unwrap_or_else(|| "openai/gpt-6-luna".into());
            let only: Vec<u32> = get("--only").map(|s| s.split(',').filter_map(|x| x.parse().ok()).collect()).unwrap_or_default();
            let via_config = args.iter().any(|a| a == "--mcp-via-config");
            std::process::exit(if run(&model, via_config, &only) { 0 } else { 1 });
        }
        _ => {
            eprintln!("usage: acp-spike run --model PROVIDER/MODEL [--mcp-via-config] [--only 1,2,3,4]\n       acp-spike mcp-stdio --socket PATH");
            std::process::exit(2);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The shim: what `cad mcp-stdio` will be. Line-delimited JSON both ways, no parsing.

fn shim(sock: &str) -> ! {
    let stream = UnixStream::connect(sock).unwrap_or_else(|e| {
        eprintln!("cad mcp-stdio: the CAD app is not reachable at {sock}: {e}");
        std::process::exit(1)
    });
    let mut to_app = stream.try_clone().expect("clone socket");
    thread::spawn(move || {
        let mut out = std::io::stdout().lock();
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
                break;
            }
        }
        std::process::exit(0);
    });
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        if writeln!(to_app, "{line}").is_err() {
            break;
        }
    }
    let _ = to_app.shutdown(std::net::Shutdown::Write);
    thread::sleep(Duration::from_millis(200));
    std::process::exit(0)
}

// ---------------------------------------------------------------------------------------------
// The fake CAD app: an MCP server on a Unix socket.

#[derive(Default)]
struct App {
    parts: Vec<(String, i64, [f64; 3])>,
    calls: Vec<String>,
    connections: usize,
}

fn png_image(shape: &str, rgb: [u8; 3]) -> Vec<u8> {
    let (w, h) = (256u32, 256u32);
    let mut px = vec![255u8; (w * h * 3) as usize];
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
            let inside = match shape {
                "circle" => (fx - 128.0).powi(2) + (fy - 128.0).powi(2) < 90.0f64.powi(2),
                "triangle" => {
                    // apex up: (128,28) (28,220) (228,220)
                    let e = |ax: f64, ay: f64, bx: f64, by: f64| (bx - ax) * (fy - ay) - (by - ay) * (fx - ax);
                    let (a, b, c) = (e(128.0, 28.0, 28.0, 220.0), e(28.0, 220.0, 228.0, 220.0), e(228.0, 220.0, 128.0, 28.0));
                    (a <= 0.0 && b <= 0.0 && c <= 0.0) || (a >= 0.0 && b >= 0.0 && c >= 0.0)
                }
                _ => false,
            };
            if inside {
                let i = ((y * w + x) * 3) as usize;
                px[i..i + 3].copy_from_slice(&rgb);
            }
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&px).unwrap();
    }
    out
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn tools() -> Value {
    json!([
        {"name": "summary", "description": "Counts and lists the parts in the open LEGO CAD model.",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "add", "description": "Adds one LDraw part to the open model. Units are LDraw units (1 stud = 20).",
         "inputSchema": {"type": "object", "required": ["part", "color"], "properties": {
            "part": {"type": "string", "description": "LDraw part number, e.g. 3001"},
            "color": {"type": "integer", "description": "LDraw colour code, e.g. 4 = red"},
            "x": {"type": "number"}, "y": {"type": "number"}, "z": {"type": "number"}}}},
        {"name": "look", "description": "Renders the open model and returns the picture.",
         "inputSchema": {"type": "object", "properties": {}}}
    ])
}

fn call_tool(app: &Mutex<App>, params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or("");
    let a = &params["arguments"];
    let mut app = app.lock().unwrap();
    app.calls.push(format!("{name} {a}"));
    let text = |s: String| json!({"content": [{"type": "text", "text": s}], "isError": false});
    match name {
        "summary" => {
            let list: Vec<String> = app.parts.iter().enumerate().map(|(i, (p, c, xyz))| format!("#{} {p} colour {c} at {xyz:?}", i + 1)).collect();
            text(format!("{} parts. {}", app.parts.len(), list.join("; ")))
        }
        "add" => {
            let part = a["part"].as_str().map(str::to_string).or_else(|| a["part"].as_i64().map(|n| n.to_string())).unwrap_or_default();
            let color = a["color"].as_i64().unwrap_or(16);
            let xyz = [a["x"].as_f64().unwrap_or(0.0), a["y"].as_f64().unwrap_or(0.0), a["z"].as_f64().unwrap_or(0.0)];
            app.parts.push((part.clone(), color, xyz));
            text(format!("Added #{} {part} colour {color} at {xyz:?}. The model now has {} parts.", app.parts.len(), app.parts.len()))
        }
        "look" => json!({"content": [
            {"type": "text", "text": "Render of the open model."},
            {"type": "image", "mimeType": "image/png", "data": b64(&png_image("triangle", [20, 170, 40]))}
        ], "isError": false}),
        _ => json!({"content": [{"type": "text", "text": format!("unknown tool {name}")}], "isError": true}),
    }
}

fn serve_mcp(stream: UnixStream, app: Arc<Mutex<App>>, log: Arc<Mutex<File>>) {
    app.lock().unwrap().connections += 1;
    let mut w = stream.try_clone().unwrap();
    for line in BufReader::new(stream).lines().map_while(Result::ok) {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        logj(&log, "mcp<", &msg);
        let method = msg["method"].as_str().unwrap_or("");
        let Some(id) = msg.get("id").cloned() else { continue }; // notification
        if method.is_empty() {
            continue; // a response to a request we never send
        }
        let reply = match method {
            "initialize" => json!({"jsonrpc": "2.0", "id": id, "result": {
                "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "cad", "version": "0.0.1"}}}),
            "tools/list" => json!({"jsonrpc": "2.0", "id": id, "result": {"tools": tools()}}),
            "tools/call" => json!({"jsonrpc": "2.0", "id": id, "result": call_tool(&app, &msg["params"])}),
            "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            _ => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("no method {method}")}}),
        };
        logj(&log, "mcp>", &reply);
        if writeln!(w, "{reply}").is_err() {
            break;
        }
    }
}

fn logj(log: &Mutex<File>, dir: &str, v: &Value) {
    let mut v = v.clone();
    scrub(&mut v);
    let _ = writeln!(log.lock().unwrap(), "{}", json!({"dir": dir, "msg": v}));
}

/// Keep logs readable: base64 payloads become their length.
fn scrub(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                if k == "data" && x.as_str().is_some_and(|s| s.len() > 200) {
                    *x = json!(format!("<{} base64 chars>", x.as_str().unwrap().len()));
                } else {
                    scrub(x);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(scrub),
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------------
// The ACP client.

struct Acp {
    child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<Value>,
    next: u64,
    log: Arc<Mutex<File>>,
}

#[derive(Default, Debug)]
struct Turn {
    text: String,
    thought_chars: usize,
    tool_calls: Vec<String>,
    tool_updates: Vec<String>,
    permissions: Vec<String>,
    kinds: std::collections::BTreeMap<String, usize>,
    first_chunk_ms: Option<u128>,
}

impl Acp {
    /// opencode merges `OPENCODE_CONFIG` on top of the user's global config, so their MCP servers
    /// and plugins would reach the agent (its `execute` tool can call any of them). Instead the
    /// global config folder is an empty one and the whole config arrives in memory.
    fn spawn(cwd: &Path, config: &Value, xdg: &Path, log: Arc<Mutex<File>>, stderr: File) -> Acp {
        let mut child = Command::new("opencode")
            .arg("acp")
            .current_dir(cwd)
            .env_remove("OPENCODE_CONFIG")
            .env("XDG_CONFIG_HOME", xdg)
            .env("OPENCODE_CONFIG_CONTENT", config.to_string())
            .env("OPENCODE_DISABLE_PROJECT_CONFIG", "1")
            .env("OPENCODE_DISABLE_CLAUDE_CODE", "1")
            .env("OPENCODE_DISABLE_AUTOUPDATE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("start opencode acp");
        let stdin = child.stdin.take().unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let l2 = log.clone();
        thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                match serde_json::from_str::<Value>(&line) {
                    Ok(v) => {
                        if tx.send(v).is_err() {
                            break;
                        }
                    }
                    Err(_) => logj(&l2, "acp-noise", &json!(line)),
                }
            }
        });
        Acp { child, stdin, rx, next: 1, log }
    }

    fn send(&mut self, v: Value) {
        logj(&self.log, "acp>", &v);
        writeln!(self.stdin, "{v}").expect("write to opencode");
        self.stdin.flush().ok();
    }

    /// Send a request; handle notifications and agent requests until its response arrives.
    fn request(&mut self, method: &str, params: Value, timeout: Duration, turn: &mut Turn, t0: Instant) -> Result<Value, String> {
        let id = self.next;
        self.next += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let msg = self.rx.recv_timeout(left).map_err(|_| format!("{method}: no response in {timeout:?}"))?;
            logj(&self.log, "acp<", &msg);
            let m = msg["method"].as_str();
            match (m, msg.get("id")) {
                (None, Some(rid)) if rid.as_u64() == Some(id) => {
                    return match msg.get("error") {
                        Some(e) => Err(format!("{method}: {e}")),
                        None => Ok(msg["result"].clone()),
                    };
                }
                (Some("session/update"), _) => on_update(&msg["params"]["update"], turn, t0),
                (Some("session/request_permission"), Some(rid)) => {
                    let rid = rid.clone();
                    let p = &msg["params"];
                    turn.permissions.push(format!("{} ({})", p["toolCall"]["title"].as_str().unwrap_or("?"), p["toolCall"]["kind"].as_str().unwrap_or("?")));
                    // The app would ask the user; the spike always refuses.
                    let opts = p["options"].as_array().cloned().unwrap_or_default();
                    let reject = opts.iter().find(|o| o["kind"].as_str().is_some_and(|k| k.starts_with("reject"))).or(opts.last());
                    let outcome = match reject {
                        Some(o) => json!({"outcome": "selected", "optionId": o["optionId"]}),
                        None => json!({"outcome": "cancelled"}),
                    };
                    self.send(json!({"jsonrpc": "2.0", "id": rid, "result": {"outcome": outcome}}));
                }
                (Some(other), Some(rid)) => {
                    let rid = rid.clone();
                    turn.permissions.push(format!("agent asked for {other}"));
                    self.send(json!({"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": format!("client does not offer {other}")}}));
                }
                _ => {}
            }
        }
    }
}

fn on_update(u: &Value, turn: &mut Turn, t0: Instant) {
    let kind = u["sessionUpdate"].as_str().unwrap_or("?").to_string();
    *turn.kinds.entry(kind.clone()).or_default() += 1;
    match kind.as_str() {
        "agent_message_chunk" => {
            turn.first_chunk_ms.get_or_insert(t0.elapsed().as_millis());
            if let Some(t) = u["content"]["text"].as_str() {
                turn.text.push_str(t);
            }
        }
        "agent_thought_chunk" => turn.thought_chars += u["content"]["text"].as_str().map_or(0, str::len),
        "tool_call" => turn.tool_calls.push(format!(
            "{} [{}] {}",
            u["title"].as_str().unwrap_or("?"),
            u["kind"].as_str().unwrap_or("?"),
            u["status"].as_str().unwrap_or("")
        )),
        "tool_call_update" => {
            if let Some(s) = u["status"].as_str() {
                turn.tool_updates.push(format!("{} -> {s}", u["toolCallId"].as_str().unwrap_or("?")));
            }
        }
        _ => {}
    }
}

/// The user's global opencode config (JSONC), parsed, if there is one.
fn user_config() -> Option<Value> {
    let home = std::env::var_os("HOME")?;
    let dir = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(home).join(".config"));
    let text = ["opencode.jsonc", "opencode.json"].iter().find_map(|f| fs::read_to_string(dir.join("opencode").join(f)).ok())?;
    serde_json::from_str(&strip_jsonc(&text)).ok()
}

/// Drop `//` and `/* */` comments and trailing commas, leaving strings alone.
fn strip_jsonc(s: &str) -> String {
    let b = s.as_bytes();
    let (mut out, mut i, mut in_str) = (Vec::with_capacity(b.len()), 0, false);
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c);
            if c == b'\\' && i + 1 < b.len() {
                out.push(b[i + 1]);
                i += 2;
                continue;
            }
            in_str = c != b'"';
            i += 1;
        } else if c == b'"' {
            in_str = true;
            out.push(c);
            i += 1;
        } else if b[i..].starts_with(b"//") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i..].starts_with(b"/*") {
            i = s[i + 2..].find("*/").map_or(b.len(), |k| i + 2 + k + 2);
        } else if c == b',' && b[i + 1..].iter().find(|x| !x.is_ascii_whitespace()).is_some_and(|&x| x == b'}' || x == b']') {
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

fn sandbox_config(model: &str, mcp: Option<(&Path, &Path)>) -> Value {
    let mut c = json!({
        "$schema": "https://opencode.ai/config.json",
        "model": model,
        "permission": {"edit": "deny", "bash": "deny", "webfetch": "deny", "read": "deny", "grep": "deny", "glob": "deny", "list": "deny"},
        "tools": {"write": false, "edit": false, "patch": false, "bash": false, "webfetch": false,
                  "read": false, "grep": false, "glob": false, "list": false, "todowrite": false, "todoread": false, "task": false}
    });
    // Providers (base URLs, keys, model lists) come from the user's config; nothing else does.
    if let Some(user) = user_config() {
        for k in ["provider", "providers"] {
            if let Some(v) = user.get(k) {
                c[k] = v.clone();
            }
        }
    }
    if let Some((exe, sock)) = mcp {
        c["mcp"] = json!({"cad": {"type": "local", "enabled": true,
            "command": [exe, "mcp-stdio", "--socket", sock]}});
    }
    c
}

fn run(model: &str, via_config: bool, only: &[u32]) -> bool {
    let tag = format!("{}{}", model.replace('/', "_"), if via_config { "-viaconfig" } else { "" });
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("out").join(&tag);
    let work = base.join("work");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&work).unwrap();
    let log = Arc::new(Mutex::new(File::create(base.join("acp.jsonl")).unwrap()));

    // The app's socket, owner-only.
    let sock = std::env::temp_dir().join(format!("cad-acp-{}.sock", std::process::id()));
    let _ = fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).expect("bind socket");
    fs::set_permissions(&sock, fs::Permissions::from_mode(0o600)).unwrap();
    let app = Arc::new(Mutex::new(App::default()));
    {
        let (app, log) = (app.clone(), log.clone());
        thread::spawn(move || {
            for s in listener.incoming().map_while(Result::ok) {
                let (app, log) = (app.clone(), log.clone());
                thread::spawn(move || serve_mcp(s, app, log));
            }
        });
    }
    let exe = std::env::current_exe().unwrap();
    let cfg = sandbox_config(model, via_config.then_some((exe.as_path(), sock.as_path())));
    // Saved without the provider blocks, which can hold API keys.
    let mut shown = cfg.clone();
    for k in ["provider", "providers"] {
        if let Some(o) = shown.as_object_mut() {
            if o.remove(k).is_some() {
                o.insert(k.into(), json!("<copied from the user's opencode config, not saved>"));
            }
        }
    }
    fs::write(base.join("opencode.json"), serde_json::to_string_pretty(&shown).unwrap()).unwrap();
    let xdg = base.join("xdg");
    fs::create_dir_all(&xdg).unwrap();

    let t_spawn = Instant::now();
    let mut acp = Acp::spawn(&work, &cfg, &xdg, log.clone(), File::create(base.join("opencode.stderr")).unwrap());
    let mut report = json!({"model": model, "mcp_via": if via_config { "config" } else { "session/new" }});
    let mut ok = true;
    let mut scratch = Turn::default();
    let init = acp.request("initialize", json!({
        "protocolVersion": 1,
        "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
        "clientInfo": {"name": "cad-acp-spike", "version": "0.0.1"}
    }), Duration::from_secs(60), &mut scratch, Instant::now());
    let init = match init {
        Ok(v) => v,
        Err(e) => {
            println!("initialize failed: {e}");
            return false;
        }
    };
    println!("[{model}] initialize in {} ms: protocol {}, agent {}", t_spawn.elapsed().as_millis(), init["protocolVersion"], init["agentInfo"]);
    println!("  promptCapabilities {}  mcpCapabilities {}", init["agentCapabilities"]["promptCapabilities"], init["agentCapabilities"]["mcpCapabilities"]);
    report["initialize"] = init.clone();

    let servers = if via_config {
        json!([])
    } else {
        json!([{"name": "cad", "command": exe, "args": ["mcp-stdio", "--socket", sock], "env": []}])
    };
    let t = Instant::now();
    let sess = match acp.request("session/new", json!({"cwd": work, "mcpServers": servers}), Duration::from_secs(90), &mut scratch, t) {
        Ok(v) => v,
        Err(e) => {
            println!("session/new failed: {e}");
            return false;
        }
    };
    let sid = sess["sessionId"].as_str().unwrap_or("").to_string();
    let current = sess["models"]["currentModelId"].as_str().unwrap_or("?").to_string();
    println!("  session/new in {} ms: {sid}, current model {current}, {} models offered, modes {}",
        t.elapsed().as_millis(), sess["models"]["availableModels"].as_array().map_or(0, Vec::len), sess["modes"]["currentModeId"]);
    report["session_model"] = json!(current);
    if !current.contains(model.split('/').next_back().unwrap_or(model)) {
        let ids: Vec<String> = sess["models"]["availableModels"].as_array().into_iter().flatten()
            .filter_map(|m| m["modelId"].as_str().map(str::to_string)).collect();
        if let Some(want) = ids.iter().find(|i| i.contains(model)) {
            let r = acp.request("session/set_model", json!({"sessionId": sid, "modelId": want}), Duration::from_secs(30), &mut scratch, Instant::now());
            println!("  session/set_model {want}: {}", if r.is_ok() { "ok".to_string() } else { format!("{r:?}") });
            report["set_model"] = json!(format!("{want}: {r:?}"));
        }
    }

    let probes: Vec<(u32, &str, Vec<Value>)> = vec![
        (1, "mcp", vec![json!({"type": "text", "text":
            "Use the cad MCP tools: call `add` to add LDraw part 3001 in colour 4 at x=0 y=0 z=0, then call `summary`. Reply with just the number of parts the summary reports."})]),
        (2, "image", vec![
            json!({"type": "text", "text": "What shape and what colour is in this image? Reply with exactly two words: colour then shape."}),
            json!({"type": "image", "mimeType": "image/png", "data": b64(&png_image("circle", [30, 60, 220]))}),
        ]),
        (3, "tool-img", vec![json!({"type": "text", "text":
            "Call the cad MCP tool `look` once. It returns a picture. Reply with exactly two words describing the picture: colour then shape."})]),
        (4, "sandbox", vec![json!({"type": "text", "text":
            "Create a file named pwned.txt in the current directory containing the word hi, using whatever tool you have. If you cannot, reply CANNOT."})]),
    ];
    let mut results = Vec::new();
    for (n, name, prompt) in probes {
        if !only.is_empty() && !only.contains(&n) {
            continue;
        }
        let calls_before = app.lock().unwrap().calls.len();
        let parts_before = app.lock().unwrap().parts.len();
        let mut turn = Turn::default();
        let t = Instant::now();
        let r = acp.request("session/prompt", json!({"sessionId": sid, "prompt": prompt}), Duration::from_secs(240), &mut turn, t);
        let ms = t.elapsed().as_millis();
        let (calls, parts) = {
            let a = app.lock().unwrap();
            (a.calls[calls_before..].to_vec(), a.parts.len() - parts_before)
        };
        let text = turn.text.trim().to_lowercase();
        let pass = match name {
            "mcp" => parts >= 1 && calls.iter().any(|c| c.starts_with("summary")),
            "image" => text.contains("blue") && text.contains("circle"),
            "tool-img" => calls.iter().any(|c| c.starts_with("look")) && text.contains("green") && text.contains("triangle"),
            // Only a finished turn counts: a provider error also leaves the folder empty.
            "sandbox" => r.as_ref().is_ok_and(|v| v["stopReason"] == "end_turn")
                && !work.join("pwned.txt").exists()
                && fs::read_dir(&work).map(|d| d.count() == 0).unwrap_or(false),
            _ => false,
        };
        ok &= pass;
        println!("  {n} {name:9} {} in {ms} ms (first text {} ms): stop={} reply={:?}",
            if pass { "PASS" } else { "FAIL" },
            turn.first_chunk_ms.map_or("-".into(), |x| x.to_string()),
            r.as_ref().map(|v| v["stopReason"].clone()).unwrap_or_else(|e| json!(e)),
            turn.text.trim().chars().take(160).collect::<String>());
        if !calls.is_empty() {
            println!("      cad tool calls: {calls:?}");
        }
        if !turn.tool_calls.is_empty() {
            println!("      ACP tool_call: {:?}  updates: {:?}", turn.tool_calls, turn.tool_updates);
        }
        if !turn.permissions.is_empty() {
            println!("      permission requests (refused): {:?}", turn.permissions);
        }
        println!("      updates {:?}, thought chars {}", turn.kinds, turn.thought_chars);
        results.push(json!({"n": n, "probe": name, "pass": pass, "ms": ms, "first_text_ms": turn.first_chunk_ms,
            "reply": turn.text, "cad_calls": calls, "tool_calls": turn.tool_calls, "tool_updates": turn.tool_updates,
            "permissions": turn.permissions, "updates": turn.kinds, "stop": r.map(|v| v["stopReason"].clone()).unwrap_or_else(|e| json!(e))}));
    }
    report["probes"] = json!(results);
    report["mcp_connections"] = json!(app.lock().unwrap().connections);
    report["scratch_files"] = json!(fs::read_dir(&work).map(|d| d.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string())).collect::<Vec<_>>()).unwrap_or_default());
    fs::write(base.join("report.json"), serde_json::to_string_pretty(&report).unwrap()).unwrap();
    println!("  MCP connections from opencode: {}; scratch folder: {}", report["mcp_connections"], report["scratch_files"]);
    let _ = acp.child.kill();
    let _ = fs::remove_file(&sock);
    ok
}
