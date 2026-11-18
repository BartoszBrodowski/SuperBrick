//! The two front ends over the same tools: the `sb` command line and `sb mcp`.
//!
//! ```text
//! sb --new NAME [--import MODEL.ldr]             create NAME.sb
//! sb P.sb --user U --op '{"tool":"edit",...}'    run one operation (or TOOL 'JSON')
//! sb P.sb --user U --ops @ops.json               run a list of operations in order
//! sb P.sb export [-o OUT.ldr] [--force]          write the working model as LDraw
//! sb mcp --user U [P.sb]                         MCP server on stdio
//! sb tools [TOOL]                                describe the tools
//! ```

use crate::{create_project, Reply, Session, TOOLS};
use ldraw::Library;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const NEED_USER: &str = "--user is required for project operations: agents pass their own name (--user claude); \
you pass --user human to change the model directly";

const VALUE_FLAGS: &[&str] = &["--user", "--out", "--op", "--operation", "--ops", "-o", "--output", "--branch", "--import", "--new", "--project"];

/// Positional arguments after the project path (flag values skipped).
fn positionals(argv: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut it = argv.iter().skip(1);
    while let Some(a) = it.next() {
        if VALUE_FLAGS.contains(&a.as_str()) {
            it.next();
        } else if !a.starts_with("--") || a == "-" {
            out.push(a.as_str());
        }
    }
    out
}

/// Whether these arguments are for the project front end (not the GUI or dev commands).
pub fn handles(argv: &[String]) -> bool {
    let first = argv.first().map(String::as_str).unwrap_or("");
    if matches!(first, "mcp" | "tools" | "setup") || argv.iter().any(|a| a == "--new") {
        return true;
    }
    let project = first.ends_with(".sb");
    project
        && (argv.iter().any(|a| matches!(a.as_str(), "--op" | "--operation" | "--ops" | "--user"))
            || positionals(argv).first().is_some_and(|a| *a == "export" || TOOLS.iter().any(|t| t.name == *a)))
}

/// Run the project front end; returns the process exit code.
pub fn run(argv: &[String]) -> i32 {
    match dispatch(argv) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sb: {e}");
            2
        }
    }
}

fn flag<'a>(argv: &'a [String], names: &[&str]) -> Option<&'a str> {
    argv.iter().position(|a| names.contains(&a.as_str())).and_then(|i| argv.get(i + 1)).map(String::as_str)
}

fn library() -> Result<Arc<Library>, String> {
    Library::open_default().map(Arc::new).map_err(|e| format!("could not open the LDraw library: {e}"))
}

/// `sb setup [--library] [--python]`: report, or install, what SuperBrick downloads on first run.
fn setup(argv: &[String]) -> Result<i32, String> {
    use ldraw::runtime;
    let want_library = argv.iter().any(|a| a == "--library" || a == "--all");
    let want_python = argv.iter().any(|a| a == "--python" || a == "--all");
    // --force installs SuperBrick's own copy even when one is already found.
    let force = argv.iter().any(|a| a == "--force");
    let mut report = |p: runtime::Progress| {
        if let Some(f) = p.fraction() {
            eprint!("\r{} {:>3.0}%   ", p.step, f * 100.0);
        } else {
            eprint!("\r{}   ", p.step);
        }
    };
    if want_library && (force || runtime::library_zip().is_none()) {
        let path = runtime::install_library(&mut report)?;
        eprintln!("\nLDraw library: {}", path.display());
    }
    if want_python && (force || runtime::python().is_none()) {
        let path = runtime::install_python(&mut report)?;
        eprintln!("\nPython: {}", path.display());
    }
    let library = runtime::library_zip();
    let python = runtime::python();
    println!("LDraw library: {}", library.as_ref().map_or("missing (sb setup --library)".into(), |p| p.display().to_string()));
    println!("Python (NumPy + Pillow): {}", python.as_ref().map_or("missing (sb setup --python; only needed by connect, hinge, instructions, studio)".into(), |p| p.display().to_string()));
    println!("Support folder: {}", runtime::support_dir().display());
    Ok(if library.is_some() { 0 } else { 1 })
}

fn dispatch(argv: &[String]) -> Result<i32, String> {
    let first = argv.first().map(String::as_str).unwrap_or("");
    if first == "tools" {
        return Ok(tools_help(argv.get(1).map(String::as_str)));
    }
    if first == "setup" {
        return setup(argv);
    }
    if first == "mcp" {
        let user = flag(argv, &["--user"]).ok_or(NEED_USER)?;
        let project = argv.iter().skip(1).find(|a| a.ends_with(".sb")).map(PathBuf::from);
        let project = project.or_else(|| flag(argv, &["--project"]).map(PathBuf::from));
        let any = argv.iter().any(|a| a == "--any-project") || ldraw::app_env("ANY_PROJECT").is_some_and(|v| v == "1");
        mcp(user, project, any).map(|_| 0)
    } else if let Some(name) = flag(argv, &["--new"]) {
        let path = if name.ends_with(".sb") { PathBuf::from(name) } else { PathBuf::from(format!("{name}.sb")) };
        let title = path.file_stem().and_then(|s| s.to_str()).unwrap_or("model").to_string();
        let import = flag(argv, &["--import"]).map(Path::new);
        match flag(argv, &["--user"]) {
            // A person at the terminal: the imported model becomes the shared model.
            None | Some(crate::HUMAN) => {
                if import.is_some() {
                    crate::human_allowed().map_err(|_| "an agent's import needs --user NAME (it arrives as a draft for the human to accept)".to_string())?;
                }
                create_project(&path, &title, import)?;
            }
            Some(user) if crate::valid_user(user) => crate::create_project_as(&path, &title, import, user, library()?)?,
            Some(user) => return Err(format!("invalid user name {user:?}")),
        }
        println!("{}", json!({"created": path, "title": title}));
        Ok(0)
    } else if positionals(argv).first() == Some(&"export") {
        let path = PathBuf::from(first);
        let store = store::Store::open(&path)?;
        let out = flag(argv, &["-o", "--output"]).map(PathBuf::from).unwrap_or_else(|| {
            PathBuf::from(format!("{}.ldr", path.file_stem().and_then(|s| s.to_str()).unwrap_or("model")))
        });
        if out.exists() && !argv.iter().any(|a| a == "--force") {
            return Err(format!("{} exists (pass --force to overwrite)", out.display()));
        }
        let branch = flag(argv, &["--branch"]).unwrap_or("main");
        // With --user NAME (an agent), export what that agent sees: the model with its drafts.
        let bytes = match flag(argv, &["--user"]) {
            Some(user) if user != crate::HUMAN && branch == "main" => {
                drop(store);
                let lib = std::sync::Arc::new(ldraw::Library::open_default().map_err(|e| e.to_string())?);
                crate::Session::open(&path, user, lib)?.export()?
            }
            _ => store.export_ldr(branch)?,
        };
        std::fs::write(&out, bytes).map_err(|e| e.to_string())?;
        println!("{}", json!({"exported": out}));
        Ok(0)
    } else {
        operations(argv)
    }
}

fn tools_help(tool: Option<&str>) -> i32 {
    match tool {
        Some(name) => match TOOLS.iter().find(|t| t.name == name) {
            Some(t) => {
                println!("{}\n\n{}", t.summary, serde_json::to_string_pretty(&(t.schema)()).unwrap_or_default());
                0
            }
            None => {
                eprintln!("sb: no tool {name:?}");
                2
            }
        },
        None => {
            for t in TOOLS {
                println!("{:8} {}", t.name, t.summary);
            }
            println!("\nsb tools TOOL shows its arguments. Run one: sb P.sb --user NAME --op '{{\"tool\":\"look\"}}'");
            0
        }
    }
}

/// `{"tool":"edit","args":{…}}`, `{"tool":"edit",…args}` or `{"edit":{…}}` → (tool, args).
pub fn parse_operation(v: &Value) -> Result<(String, Value), String> {
    let obj = v.as_object().ok_or("an operation is a JSON object")?;
    if let Some(tool) = obj.get("tool").and_then(Value::as_str) {
        let args = match obj.get("args") {
            Some(a) => a.clone(),
            None => {
                let mut rest = obj.clone();
                rest.remove("tool");
                Value::Object(rest)
            }
        };
        return Ok((tool.to_string(), args));
    }
    if obj.len() == 1 {
        let (k, a) = obj.iter().next().unwrap();
        if TOOLS.iter().any(|t| t.name == k) {
            return Ok((k.clone(), a.clone()));
        }
    }
    Err("an operation names its tool: {\"tool\":\"edit\",\"ops\":[…]}".into())
}

fn read_json(text: &str) -> Result<Value, String> {
    let text = match text.strip_prefix('@') {
        Some(file) if file == "-" => {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut s).map_err(|e| e.to_string())?;
            s
        }
        Some(file) => std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?,
        None => text.to_string(),
    };
    serde_json::from_str(&text).map_err(|e| format!("bad JSON: {e}"))
}

fn operations(argv: &[String]) -> Result<i32, String> {
    let path = PathBuf::from(&argv[0]);
    let user = flag(argv, &["--user"]).ok_or(NEED_USER)?;
    if user == crate::HUMAN {
        crate::human_allowed()?;
    }
    let mut ops: Vec<(String, Value)> = Vec::new();
    if let Some(op) = flag(argv, &["--op", "--operation"]) {
        ops.push(parse_operation(&read_json(op)?)?);
    }
    if let Some(list) = flag(argv, &["--ops"]) {
        match read_json(list)? {
            Value::Array(a) => {
                for v in &a {
                    ops.push(parse_operation(v)?);
                }
            }
            v => ops.push(parse_operation(&v)?),
        }
    }
    let pos = positionals(argv);
    if let Some(tool) = pos.first() {
        if !TOOLS.iter().any(|t| t.name == *tool) {
            return Err(format!("unknown tool {tool:?} (sb tools lists them)"));
        }
        let args = match pos.get(1) {
            Some(j) => read_json(j)?,
            None => json!({}),
        };
        ops.push((tool.to_string(), args));
    }
    if ops.is_empty() {
        return Err("nothing to do: pass --op '{\"tool\":…}', --ops @file.json or TOOL 'JSON'".into());
    }
    if !path.exists() {
        return Err(format!("{} does not exist (create it with sb --new NAME)", path.display()));
    }
    let images = !argv.iter().any(|a| a == "--no-images");
    let out = flag(argv, &["--out"]).map(PathBuf::from).unwrap_or_else(|| {
        std::env::temp_dir()
            .join("sb-out")
            .join(path.file_stem().and_then(|s| s.to_str()).unwrap_or("project"))
    });
    let mut session = Session::open(&path, user, library()?)?;
    session.images = images;
    let mut failed = false;
    for (i, (tool, args)) in ops.iter().enumerate() {
        let reply = session.call(tool, args);
        failed |= !reply.ok;
        let paths = write_images(&out, &reply, i, tool)?;
        let mut v = reply.to_json(&paths);
        v["tool"] = json!(tool);
        println!("{v}");
    }
    Ok(if failed { 1 } else { 0 })
}

fn write_images(dir: &Path, reply: &Reply, index: usize, tool: &str) -> Result<Vec<PathBuf>, String> {
    if reply.images.is_empty() {
        return Ok(Vec::new());
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut paths = Vec::new();
    for (k, img) in reply.images.iter().enumerate() {
        let p = dir.join(format!("{stamp}-{index}-{tool}-{k}.png"));
        std::fs::write(&p, &img.png).map_err(|e| e.to_string())?;
        paths.push(p);
    }
    prune_images(dir);
    Ok(paths)
}

/// Keep only the newest pictures `sb` wrote in `dir` (`SB_KEEP_IMAGES`, default 40), so a
/// long agent run never fills the disk. Only files named like ours are touched.
fn prune_images(dir: &Path) {
    let keep = ldraw::app_env("KEEP_IMAGES")
        .and_then(|v| v.to_str()?.parse::<usize>().ok())
        .unwrap_or(40);
    let ours = |name: &str| {
        let Some(stem) = name.strip_suffix(".png") else { return false };
        let parts: Vec<&str> = stem.split('-').collect();
        parts.len() == 4
            && [0, 1, 3].iter().all(|&i| !parts[i].is_empty() && parts[i].bytes().all(|b| b.is_ascii_digit()))
            && parts[2].bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
    };
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            ours(&name).then(|| (name, e.path()))
        })
        .collect();
    if files.len() <= keep {
        return;
    }
    // Names start with a millisecond stamp, so sorting by (stamp, rest) is oldest first.
    files.sort_by_key(|(n, _)| {
        let stamp: u128 = n.split('-').next().and_then(|s| s.parse().ok()).unwrap_or(0);
        (stamp, n.clone())
    });
    let excess = files.len() - keep;
    for (_, path) in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

// ---------------------------------------------------------------- MCP

const INSTRUCTIONS: &str = "SuperBrick: a LEGO CAD project (.sb) you edit with tools that answer with pictures.
- Open or create a project first: project {action:\"open\", path:\"x.sb\", create?:true, import?:\"model.ldr\"}; later calls use it (or pass project:\"x.sb\").
- Coordinates are LDraw units (1 stud = 20, 1 plate = 8, 1 brick = 24) with y pointing DOWN; units:\"grid\" gives studs/plates with y up. A part's position is its LDraw origin (bricks: centre of the top face).
- Rotations are 9 numbers in LDraw row order (as written in .ldr files) or shorthands like \"yaw90\", \"pitch-90*yaw180\".
- Parts have stable ids (#123). Selections (\"s4\"), candidate sets (\"c2\") and \"last\" (what your previous edit made) work anywhere ids are accepted.
- Your edits are drafts: they build on each other and the human accepts or rejects them in the SuperBrick window. Rejection notes come back to you in later replies. Drafts persist in the project file.
- Every edit is collision-checked and answers with a picture from where the change is visible; a failed batch changes nothing and shows why. Use where to find legal spots, search to find parts, verify for floating parts and weak joints.";

/// One MCP server can serve several agents at once (opencode runs one for all its sessions): each
/// call names its agent with `as` (default: the server's `--user`). Every agent works in one
/// project, and one agent's calls run in order while different agents' calls run side by side.
struct Server {
    user: String,
    /// Let an agent open more than one project (`sb mcp --any-project`); normally it keeps to the first.
    any_project: bool,
    lib: Mutex<Option<Arc<Library>>>,
    sessions: Mutex<HashMap<(String, PathBuf), Arc<Mutex<Session>>>>,
    /// The project each agent works in.
    current: Mutex<HashMap<String, PathBuf>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The agent a tools/call message is for (routes it to that agent's queue).
fn agent_of(msg: &Value, default: &str) -> String {
    msg.pointer("/params/arguments/as").and_then(Value::as_str).unwrap_or(default).to_string()
}

impl Server {
    fn new(user: &str, any_project: bool) -> Server {
        Server {
            user: user.to_string(),
            any_project,
            lib: Mutex::new(None),
            sessions: Mutex::new(HashMap::new()),
            current: Mutex::new(HashMap::new()),
        }
    }

    fn lib(&self) -> Result<Arc<Library>, String> {
        let mut lib = lock(&self.lib);
        if lib.is_none() {
            *lib = Some(library()?);
        }
        Ok(lib.clone().unwrap())
    }

    /// The agent this call acts for: `as`, else the server's `--user`. Never the human.
    fn agent(&self, args: &mut Value) -> Result<String, String> {
        match args.as_object_mut().and_then(|o| o.remove("as")) {
            None => Ok(self.user.clone()),
            Some(Value::String(n)) if crate::valid_user(&n) && n != crate::HUMAN => Ok(n),
            Some(v) => Err(format!("as: {v} is not an agent name (letters, digits, - _ . ; not {:?})", crate::HUMAN)),
        }
    }

    fn open(&self, user: &str, path: &Path, create: bool, import: Option<&Path>) -> Result<PathBuf, String> {
        let key = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => {
                let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
                let parent = parent.canonicalize().map_err(|e| format!("{}: {e}", parent.display()))?;
                parent.join(path.file_name().ok_or("open needs a file path")?)
            }
        };
        if !self.any_project {
            if let Some(cur) = lock(&self.current).get(user).filter(|c| **c != key) {
                return Err(format!(
                    "{user} already works in {}. Each agent builds in one project: make other pieces as sub-builds inside it \
                     (build them aside, group them, then where/place them; or edit op subbuild) instead of a second project.",
                    cur.display()
                ));
            }
        }
        if !key.exists() {
            if !create && import.is_none() {
                return Err(format!("{} does not exist; pass create:true to make it", path.display()));
            }
            let title = key.file_stem().and_then(|s| s.to_str()).unwrap_or("model").to_string();
            crate::create_project_as(&key, &title, import, user, self.lib()?)?;
        }
        let id = (user.to_string(), key.clone());
        if !lock(&self.sessions).contains_key(&id) {
            let s = Session::open(&key, user, self.lib()?)?;
            lock(&self.sessions).insert(id, Arc::new(Mutex::new(s)));
        }
        lock(&self.current).insert(user.to_string(), key.clone());
        Ok(key)
    }

    fn session(&self, user: &str, key: &Path) -> Result<Arc<Mutex<Session>>, String> {
        lock(&self.sessions).get(&(user.to_string(), key.to_path_buf())).cloned().ok_or_else(|| format!("{} is not open", key.display()))
    }

    fn no_project(&self, user: &str) -> String {
        let others: Vec<String> = lock(&self.current).keys().filter(|u| *u != user).map(|u| format!("{u:?}")).collect();
        let mut text = format!("no project is open for {user:?}: call project {{action:\"open\", path:\"model.sb\", create:true}}");
        if !others.is_empty() {
            text += &format!(" (projects are open as {}: if that was you, pass the same \"as\" on every call)", others.join(", "));
        }
        text
    }

    fn call(&self, tool: &str, mut args: Value) -> Reply {
        let started = std::time::Instant::now();
        let result = (|| -> Result<Reply, String> {
            if !TOOLS.iter().any(|t| t.name == tool) {
                return Err(format!("unknown tool {tool:?}"));
            }
            let user = self.agent(&mut args)?;
            let project = args.as_object_mut().and_then(|o| o.remove("project")).and_then(|v| v.as_str().map(PathBuf::from));
            let key = if tool == "project" && args.get("action").and_then(Value::as_str) == Some("open") {
                let path = args.get("path").and_then(Value::as_str).map(PathBuf::from).or(project).ok_or("open needs path")?;
                let create = args.get("create").and_then(Value::as_bool).unwrap_or(false);
                let import = args.get("import").and_then(Value::as_str).map(PathBuf::from);
                let key = self.open(&user, &path, create, import.as_deref())?;
                args = json!({"action": "info"});
                key
            } else if let Some(p) = project {
                self.open(&user, &p, false, None)?
            } else {
                let current = lock(&self.current).get(&user).cloned();
                current.ok_or_else(|| self.no_project(&user))?
            };
            let s = self.session(&user, &key)?;
            let mut s = lock(&s);
            Ok(s.call(tool, &args))
        })();
        let mut reply = result.unwrap_or_else(|e| Reply::fail(e, json!({})));
        let ms = started.elapsed().as_millis() as u64;
        if let Some(o) = reply.data.as_object_mut() {
            o.insert("ms".into(), json!(ms));
        }
        if ms >= 5000 {
            reply.text += &format!("\n(took {:.1} s)", ms as f64 / 1000.0);
        }
        reply
    }

    /// Open projects and each agent's project, for a reload.
    fn state(&self) -> (Vec<(String, PathBuf)>, Vec<(String, PathBuf)>) {
        let open = lock(&self.sessions).keys().cloned().collect();
        let current = lock(&self.current).iter().map(|(u, p)| (u.clone(), p.clone())).collect();
        (open, current)
    }

    fn restore(&self, open: &[(String, PathBuf)], current: &[(String, PathBuf)]) {
        for (user, path) in open {
            if let Err(e) = self.open(user, path, false, None) {
                eprintln!("sb mcp: could not reopen {} for {user}: {e}", path.display());
            }
        }
        let mut cur = lock(&self.current);
        cur.clear();
        for (user, path) in current {
            if lock(&self.sessions).contains_key(&(user.clone(), path.clone())) {
                cur.insert(user.clone(), path.clone());
            }
        }
    }
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// The MCP `content` for a reply: the text, compact data when small, then the pictures.
pub fn mcp_content(reply: &Reply) -> Vec<Value> {
    let mut content = vec![json!({"type": "text", "text": reply.text})];
    if !reply.data.is_null() && reply.data != json!({}) {
        let data = reply.data.to_string();
        if data.len() <= 3000 {
            content.push(json!({"type": "text", "text": data}));
        }
    }
    for img in &reply.images {
        if !img.caption.is_empty() {
            content.push(json!({"type": "text", "text": format!("[picture] {}", img.caption)}));
        }
        content.push(json!({"type": "image", "data": base64(&img.png), "mimeType": "image/png"}));
    }
    content
}

fn tool_list() -> Value {
    let tools: Vec<Value> = TOOLS
        .iter()
        .map(|t| {
            let mut schema = (t.schema)();
            if let Some(props) = schema.get_mut("properties").and_then(Value::as_object_mut) {
                props.insert(
                    "project".into(),
                    json!({"type": "string", "description": "Path of your .sb project (default: the one you opened)."}),
                );
                props.insert(
                    "as".into(),
                    json!({"type": "string", "description": "Your agent name for this call (default: the server's --user). Only for an agent that needs its own name: then pass it on every call. Never \"human\"."}),
                );
                if t.name == "project" {
                    // `open` is handled by the MCP server itself (it picks the project for later calls).
                    props.insert("path".into(), json!({"type": "string", "description": "open: the .sb file to open or create."}));
                    props.insert("create".into(), json!({"type": "boolean", "description": "open: create the project if it does not exist."}));
                    props.insert("import".into(), json!({"type": "string", "description": "open with create: an .ldr/.mpd model to start from."}));
                    if let Some(actions) = props.get_mut("action").and_then(|a| a.get_mut("enum")).and_then(Value::as_array_mut) {
                        actions.insert(0, json!("open"));
                    }
                    if let Some(text) = props.get_mut("action").and_then(|a| a.get_mut("description")) {
                        *text = json!(format!("open: pick your one project for later calls (path, create, import). {}", text.as_str().unwrap_or("")));
                    }
                }
            }
            let summary = if t.name == "project" { format!("Open or create a project, then: {}", t.summary) } else { t.summary.to_string() };
            json!({"name": t.name, "description": summary, "inputSchema": schema})
        })
        .collect();
    json!({ "tools": tools })
}

fn handle(server: &Server, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2025-06-18");
            let version = if ["2024-11-05", "2025-03-26", "2025-06-18"].contains(&asked) { asked } else { "2025-06-18" };
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": true}},
                "serverInfo": {"name": "superbrick", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tool_list()),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let reply = server.call(name, args);
            Ok(json!({"content": mcp_content(&reply), "isError": !reply.ok}))
        }
        "resources/list" => Ok(json!({"resources": []})),
        "prompts/list" => Ok(json!({"prompts": []})),
        _ => Err(json!({"code": -32601, "message": format!("method not found: {method}")})),
    };
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
    })
}

/// Serve MCP on stdin/stdout until stdin closes.
///
/// A running server notices a reinstall (`tools/install.py` replacing its binary or the connector
/// and checker modules beside it) before the next tool call, and re-executes the new binary in
/// place: the client keeps its pipes and sees no restart, open projects and the agent name carry
/// over, and the new process tells the client when the tool list changed.
pub fn mcp(user: &str, project: Option<PathBuf>, any_project: bool) -> Result<(), String> {
    if !crate::valid_user(user) {
        return Err(format!("invalid user name {user:?}"));
    }
    if user == crate::HUMAN {
        crate::human_allowed()?;
        return Err(format!("sb mcp serves agents: start it with --user claude (or codex…), not {:?}", crate::HUMAN));
    }
    let server = Arc::new(Server::new(user, any_project));
    let out: Arc<Mutex<std::io::Stdout>> = Arc::new(Mutex::new(std::io::stdout()));
    let mut pending = Vec::new();
    match reload::resume() {
        Some(h) => {
            let mut open = h.agents.clone();
            let mut current = h.currents.clone();
            if open.is_empty() {
                open = h.open.iter().map(|p| (h.user.clone(), p.clone())).collect();
                current = h.current.iter().map(|p| (h.user.clone(), p.clone())).collect();
            }
            server.restore(&open, &current);
            eprintln!("sb mcp: reloaded the newly installed {}", std::env::current_exe().map(|e| e.display().to_string()).unwrap_or_default());
            if h.tools != reload::fingerprint(&tool_list().to_string()) {
                let mut o = lock(&out);
                writeln!(o, "{}", json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}))
                    .and_then(|_| o.flush())
                    .map_err(|e| e.to_string())?;
            }
            pending = h.pending;
        }
        None => {
            if let Some(p) = project {
                server.open(user, &p, false, None)?;
            }
        }
    }
    let mut watch = reload::Watch::new();
    let input = std::io::stdin();
    #[cfg(unix)]
    let input = {
        // Read through our own buffer (not the process-wide stdin one) so the bytes read ahead of
        // the current message can be handed to the reloaded process.
        use std::os::fd::AsFd;
        match input.as_fd().try_clone_to_owned() {
            Ok(fd) => Box::new(std::fs::File::from(fd)) as Box<dyn std::io::Read>,
            Err(_) => {
                watch = None;
                Box::new(input) as Box<dyn std::io::Read>
            }
        }
    };
    #[cfg(not(unix))]
    let input = {
        // Hot reload is unix only (it needs `exec`; Windows has no equivalent that keeps the same
        // process). Without a watcher `Served::Reload` is never produced, so nothing tries it. A
        // Windows version would spawn the new exe with RESUME and pipe stdin/stdout through, but
        // std's process-wide stdin buffer swallows read-ahead bytes, so it needs a raw handle
        // reader first; also a reinstall must rename the running sb.exe away (T6).
        watch = None;
        Box::new(input) as Box<dyn std::io::Read>
    };
    let mut lines = Lines::new(pending, input);
    loop {
        match serve(&server, &mut lines, &out, watch.as_mut())? {
            Served::Closed => return Ok(()),
            Served::Reload(line) => {
                let mut pending = line.into_bytes();
                pending.push(b'\n');
                pending.extend(lines.leftover());
                let (agents, currents) = server.state();
                let handoff = reload::Handoff {
                    user: server.user.clone(),
                    open: agents.iter().filter(|(u, _)| *u == server.user).map(|(_, p)| p.clone()).collect(),
                    current: currents.iter().find(|(u, _)| *u == server.user).map(|(_, p)| p.clone()),
                    tools: reload::fingerprint(&tool_list().to_string()),
                    pending,
                    agents: agents.clone(),
                    currents: currents.clone(),
                };
                // Close the projects first: sessions hand back unused ids when dropped.
                lock(&server.sessions).clear();
                let err = reload::exec(&handoff);
                // Still here: the new binary did not start. Reopen and carry on with this one.
                eprintln!("sb mcp: not reloaded: {err}");
                server.restore(&agents, &currents);
                if let Some(w) = watch.as_mut() {
                    w.accept();
                }
                lines = Lines::new(handoff.pending, Box::new(std::io::empty()) as Box<dyn std::io::Read>).then(lines);
            }
        }
    }
}

/// Lines of input: queued bytes first (handed over by a reload), then a reader.
struct Lines<R> {
    queued: std::collections::VecDeque<u8>,
    reader: std::io::BufReader<R>,
    rest: Option<Box<Lines<Box<dyn std::io::Read>>>>,
}

impl<R: std::io::Read> Lines<R> {
    fn new(queued: Vec<u8>, reader: R) -> Lines<R> {
        Lines { queued: queued.into(), reader: std::io::BufReader::new(reader), rest: None }
    }

    /// Continue with `rest` once this input is used up.
    fn then(mut self, rest: Lines<Box<dyn std::io::Read>>) -> Lines<R> {
        self.rest = Some(Box::new(rest));
        self
    }

    fn next(&mut self) -> Option<std::io::Result<String>> {
        let mut buf = Vec::new();
        while let Some(b) = self.queued.pop_front() {
            buf.push(b);
            if b == b'\n' {
                return Some(Ok(String::from_utf8_lossy(&buf).trim_end().to_string()));
            }
        }
        match self.reader.read_until(b'\n', &mut buf) {
            Ok(0) if buf.is_empty() => match self.rest.as_mut() {
                Some(rest) => rest.next(),
                None => None,
            },
            Ok(_) => Some(Ok(String::from_utf8_lossy(&buf).trim_end().to_string())),
            Err(e) => Some(Err(e)),
        }
    }

    /// Take everything read ahead but not yet served.
    fn leftover(&mut self) -> Vec<u8> {
        let mut out: Vec<u8> = self.queued.drain(..).collect();
        out.extend_from_slice(self.reader.buffer());
        let n = self.reader.buffer().len();
        self.reader.consume(n);
        if let Some(rest) = self.rest.as_mut() {
            out.extend(rest.leftover());
        }
        out
    }
}

enum Served {
    Closed,
    /// A tool call arrived after a reinstall: hand it to the new binary.
    Reload(String),
}

fn write_reply<W: Write>(output: &Mutex<W>, reply: &Value) -> Result<(), String> {
    let mut o = lock(output);
    writeln!(o, "{reply}").and_then(|_| o.flush()).map_err(|e| e.to_string())
}

/// Serve until the input closes or a reinstall needs a reload. Tool calls go to a queue per agent:
/// each agent's calls run in order, different agents' calls run at the same time, and every reply
/// is written as soon as it is ready. All calls in flight finish before this returns.
fn serve<R: std::io::Read, W: Write + Send + 'static>(
    server: &Arc<Server>,
    input: &mut Lines<R>,
    output: &Arc<Mutex<W>>,
    mut watch: Option<&mut reload::Watch>,
) -> Result<Served, String> {
    let mut queues: HashMap<String, (std::sync::mpsc::Sender<Value>, std::thread::JoinHandle<()>)> = HashMap::new();
    let finish = |queues: HashMap<String, (std::sync::mpsc::Sender<Value>, std::thread::JoinHandle<()>)>| {
        for (_, (tx, worker)) in queues {
            drop(tx);
            let _ = worker.join();
        }
    };
    while let Some(line) = input.next() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                finish(queues);
                return Err(e.to_string());
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let parsed = serde_json::from_str::<Value>(&line);
        let calls_tool = |m: &Value| m.get("method").and_then(Value::as_str) == Some("tools/call");
        let wants_tool = match &parsed {
            Ok(Value::Array(batch)) => batch.iter().any(calls_tool),
            Ok(msg) => calls_tool(msg),
            Err(_) => false,
        };
        if wants_tool && watch.as_mut().is_some_and(|w| w.changed()) {
            finish(queues);
            return Ok(Served::Reload(line));
        }
        let reply = match parsed {
            Ok(msg) if calls_tool(&msg) && msg.get("id").is_some() => {
                let agent = agent_of(&msg, &server.user);
                let (tx, _) = queues.entry(agent).or_insert_with(|| {
                    let (tx, rx) = std::sync::mpsc::channel::<Value>();
                    let (server, output) = (Arc::clone(server), Arc::clone(output));
                    let worker = std::thread::spawn(move || {
                        for msg in rx {
                            if let Some(r) = handle(&server, &msg) {
                                if let Err(e) = write_reply(&output, &r) {
                                    eprintln!("sb mcp: could not write a reply: {e}");
                                }
                            }
                        }
                    });
                    (tx, worker)
                });
                let _ = tx.send(msg);
                None
            }
            Ok(Value::Array(batch)) => {
                let out: Vec<Value> = batch.iter().filter_map(|m| handle(server, m)).collect();
                (!out.is_empty()).then(|| Value::Array(out))
            }
            Ok(msg) => handle(server, &msg),
            Err(e) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}})),
        };
        if let Some(r) = reply {
            if let Err(e) = write_reply(output, &r) {
                finish(queues);
                return Err(e);
            }
        }
    }
    finish(queues);
    Ok(Served::Closed)
}

/// Self-reload of `sb mcp` after a reinstall.
mod reload {
    use serde::{Deserialize, Serialize};
    use std::path::PathBuf;
    use std::time::SystemTime;

    const RESUME: &str = "SB_MCP_RESUME";

    /// What a reloaded server picks up from the one it replaced.
    #[derive(Serialize, Deserialize)]
    pub struct Handoff {
        pub user: String,
        pub open: Vec<PathBuf>,
        pub current: Option<PathBuf>,
        /// Fingerprint of the old tool list: the new server announces a change.
        pub tools: u64,
        /// Input read ahead but not served, starting with the tool call that triggered the reload.
        pub pending: Vec<u8>,
        /// Every agent's open projects, and the project each agent works in (newer servers).
        #[serde(default)]
        pub agents: Vec<(String, PathBuf)>,
        #[serde(default)]
        pub currents: Vec<(String, PathBuf)>,
    }

    /// FNV-1a: a fingerprint that stays the same across builds.
    pub fn fingerprint(text: &str) -> u64 {
        text.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
    }

    type Stamp = Option<(SystemTime, u64)>;

    fn stamp(path: &PathBuf) -> Stamp {
        let m = std::fs::metadata(path).ok()?;
        Some((m.modified().ok()?, m.len()))
    }

    /// The installed files whose change means new code: this binary and the Python modules of the
    /// connector and checker service. The installer copies with modification times kept, so an
    /// unchanged reinstall changes nothing.
    pub struct Watch {
        files: Vec<PathBuf>,
        seen: Vec<Stamp>,
    }

    impl Watch {
        pub fn new() -> Option<Watch> {
            if std::env::var_os("SB_MCP_NO_RELOAD").is_some() {
                return None;
            }
            let mut files = vec![std::env::current_exe().ok()?];
            if let Some(dir) = ldraw::installed_share("connectors") {
                if let Ok(entries) = std::fs::read_dir(dir.join("bench")) {
                    let mut py: Vec<PathBuf> = entries
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| p.extension().is_some_and(|x| x == "py"))
                        .collect();
                    py.sort();
                    files.extend(py);
                }
            }
            let seen = files.iter().map(stamp).collect();
            Some(Watch { files, seen })
        }

        #[cfg(test)]
        pub fn of(files: Vec<PathBuf>) -> Watch {
            let seen = files.iter().map(stamp).collect();
            Watch { files, seen }
        }

        pub fn changed(&self) -> bool {
            // A file that is missing right now is being replaced: wait for the next call.
            self.files.iter().zip(&self.seen).any(|(f, s)| {
                let now = stamp(f);
                now.is_some() && now != *s
            })
        }

        /// Take the current files as the baseline (after a reload that did not happen).
        pub fn accept(&mut self) {
            self.seen = self.files.iter().map(stamp).collect();
        }
    }

    /// Replace this process with the installed binary, same arguments, handing over `h`.
    /// Returns only when that failed.
    pub fn exec(h: &Handoff) -> String {
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(e) => return e.to_string(),
        };
        // A binary that does not start would take the server down: try it first.
        match std::process::Command::new(&exe).arg("tools").stdin(std::process::Stdio::null()).output() {
            Ok(o) if o.status.success() => {}
            Ok(o) => return format!("{} tools failed: {}", exe.display(), String::from_utf8_lossy(&o.stderr).trim()),
            Err(e) => return format!("{}: {e}", exe.display()),
        }
        let path = std::env::temp_dir().join(format!("sb-mcp-{}-{}.json", std::process::id(), h.tools));
        let data = match serde_json::to_vec(h) {
            Ok(d) => d,
            Err(e) => return e.to_string(),
        };
        if let Err(e) = std::fs::write(&path, data) {
            return format!("{}: {e}", path.display());
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let err = std::process::Command::new(&exe).args(std::env::args_os().skip(1)).env(RESUME, &path).exec();
            let _ = std::fs::remove_file(&path);
            err.to_string()
        }
        #[cfg(not(unix))]
        {
            let _ = std::fs::remove_file(&path);
            "hot reload is not supported on this platform (it needs exec); restart `sb mcp` to use a new install".into()
        }
    }

    /// The handoff left by the process this one replaced, if any.
    pub fn resume() -> Option<Handoff> {
        let path = PathBuf::from(std::env::var_os(RESUME)?);
        let data = std::fs::read(&path).ok()?;
        let _ = std::fs::remove_file(&path);
        serde_json::from_slice(&data).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_values() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn operations_parse_in_three_shapes() {
        let a = parse_operation(&json!({"tool": "look", "args": {"views": 2}})).unwrap();
        let b = parse_operation(&json!({"tool": "look", "views": 2})).unwrap();
        let c = parse_operation(&json!({"look": {"views": 2}})).unwrap();
        assert_eq!(a, b);
        assert_eq!(b, c);
        assert!(parse_operation(&json!({"views": 2})).is_err());
    }

    #[test]
    fn handles_only_project_commands() {
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(handles(&v(&["mcp", "--user", "claude"])));
        assert!(handles(&v(&["--new", "x"])));
        assert!(handles(&v(&["x.sb", "--user", "a", "look"])));
        assert!(handles(&v(&["x.sb", "export"])));
        assert!(handles(&v(&["x.sb", "--out", "d", "look"])));
        assert_eq!(positionals(&v(&["x.sb", "--user", "a", "--out", "d", "where", "{}"])), vec!["where", "{}"]);
        assert!(!handles(&v(&["x.sb"])), "plain open goes to the GUI");
        assert!(!handles(&v(&["model.ldr"])));
        assert!(!handles(&v(&["validate", "m.ldr"])));
    }

    #[test]
    fn mcp_lists_tools_and_refuses_without_project() {
        let server = Arc::new(Server::new("claude", false));
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"look","arguments":{}}}"#,
            "\n"
        );
        let out = Arc::new(Mutex::new(Vec::new()));
        let mut lines = Lines::new(Vec::new(), input.as_bytes());
        assert!(matches!(serve(&server, &mut lines, &out, None).unwrap(), Served::Closed));
        let out = std::mem::take(&mut *lock(&out));
        let lines: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 3, "the notification gets no reply");
        assert_eq!(lines[0]["result"]["serverInfo"]["name"], "superbrick");
        assert_eq!(lines[1]["result"]["tools"].as_array().unwrap().len(), TOOLS.len());
        assert_eq!(lines[2]["result"]["isError"], true);
    }

    #[test]
    fn a_reinstall_hands_the_next_tool_call_and_what_follows_to_the_new_binary() {
        let dir = std::env::temp_dir().join(format!("sb-reload-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let installed = dir.join("sb");
        std::fs::write(&installed, "old").unwrap();
        let mut watch = reload::Watch::of(vec![installed.clone()]);
        assert!(!watch.changed());
        let server = Arc::new(Server::new("claude", false));
        let call = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"look","arguments":{}}}"#;
        let after = r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#;
        let input = format!("{}\n{call}\n{after}\n", r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
        let mut lines = Lines::new(Vec::new(), input.as_bytes());
        std::fs::write(&installed, "new code").unwrap();
        assert!(watch.changed());
        let out = Arc::new(Mutex::new(Vec::new()));
        let served = serve(&server, &mut lines, &out, Some(&mut watch)).unwrap();
        let out = std::mem::take(&mut *lock(&out));
        assert_eq!(String::from_utf8(out).unwrap().lines().count(), 1, "a ping is answered by the old binary");
        let Served::Reload(line) = served else { panic!("the tool call should trigger the reload") };
        assert_eq!(line, call);
        assert_eq!(lines.leftover(), format!("{after}\n").into_bytes());
        assert!(lines.next().is_none(), "nothing is served twice");
        // A handoff that did not happen: the pending input comes first, then the rest.
        let mut retry = Lines::new(format!("{call}\n").into_bytes(), Box::new(std::io::empty()) as Box<dyn std::io::Read>)
            .then(Lines::new(b"tail\n".to_vec(), Box::new(std::io::empty()) as Box<dyn std::io::Read>));
        assert_eq!(retry.next().unwrap().unwrap(), call);
        assert_eq!(retry.next().unwrap().unwrap(), "tail");
        assert!(retry.next().is_none());
        watch.accept();
        assert!(!watch.changed());
        assert_ne!(reload::fingerprint("a"), reload::fingerprint("b"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn agents_are_named_per_call_and_keep_to_one_project() {
        let server = Arc::new(Server::new("claude", false));
        assert!(server.agent(&mut json!({"as": "human"})).is_err(), "agents never act as the human");
        assert!(server.agent(&mut json!({"as": "bad name"})).is_err());
        assert_eq!(server.agent(&mut json!({"as": "claude-roof"})).unwrap(), "claude-roof");
        assert_eq!(server.agent(&mut json!({})).unwrap(), "claude");
        let dir = std::env::temp_dir().join(format!("sb-one-project-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.sb"), dir.join("b.sb"));
        let r = server.call("project", json!({"action": "open", "path": a, "create": true}));
        assert!(r.ok, "{}", r.text);
        let r = server.call("project", json!({"action": "open", "path": b, "create": true}));
        assert!(!r.ok && r.text.contains("one project"), "{}", r.text);
        assert!(!b.exists(), "the refused project is not created");
        let r = server.call("look", json!({"project": b}));
        assert!(!r.ok, "nor reached through the project argument");
        let r = server.call("project", json!({"action": "info", "as": "codex"}));
        assert!(!r.ok && r.text.contains("\"claude\""), "another agent has no project yet: {}", r.text);
        let r = server.call("project", json!({"action": "open", "path": a, "as": "codex"}));
        assert!(r.ok, "{}", r.text);
        assert!(r.data["ms"].is_u64());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
