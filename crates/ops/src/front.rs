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
use std::sync::Arc;

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
    if matches!(first, "mcp" | "tools") || argv.iter().any(|a| a == "--new") {
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

fn dispatch(argv: &[String]) -> Result<i32, String> {
    let first = argv.first().map(String::as_str).unwrap_or("");
    if first == "tools" {
        return Ok(tools_help(argv.get(1).map(String::as_str)));
    }
    if first == "mcp" {
        let user = flag(argv, &["--user"]).ok_or(NEED_USER)?;
        let project = argv.iter().skip(1).find(|a| a.ends_with(".sb")).map(PathBuf::from);
        let project = project.or_else(|| flag(argv, &["--project"]).map(PathBuf::from));
        mcp(user, project).map(|_| 0)
    } else if let Some(name) = flag(argv, &["--new"]) {
        let path = if name.ends_with(".sb") { PathBuf::from(name) } else { PathBuf::from(format!("{name}.sb")) };
        let title = path.file_stem().and_then(|s| s.to_str()).unwrap_or("model").to_string();
        create_project(&path, &title, flag(argv, &["--import"]).map(Path::new))?;
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
        std::fs::write(&out, store.export_ldr(branch)?).map_err(|e| e.to_string())?;
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
    Ok(paths)
}

// ---------------------------------------------------------------- MCP

const INSTRUCTIONS: &str = "SuperBrick: a LEGO CAD project (.sb) you edit with tools that answer with pictures.
- Open or create a project first: project {action:\"open\", path:\"x.sb\", create?:true, import?:\"model.ldr\"}; later calls use it (or pass project:\"x.sb\").
- Coordinates are LDraw units (1 stud = 20, 1 plate = 8, 1 brick = 24) with y pointing DOWN; units:\"grid\" gives studs/plates with y up. A part's position is its LDraw origin (bricks: centre of the top face).
- Rotations are 9 numbers in LDraw row order (as written in .ldr files) or shorthands like \"yaw90\", \"pitch-90*yaw180\".
- Parts have stable ids (#123). Selections (\"s4\"), candidate sets (\"c2\") and \"last\" (what your previous edit made) work anywhere ids are accepted.
- Your edits are drafts: they build on each other and the human accepts or rejects them in the SuperBrick window. Rejection notes come back to you in later replies. Drafts persist in the project file.
- Every edit is collision-checked and answers with a picture from where the change is visible; a failed batch changes nothing and shows why. Use where to find legal spots, search to find parts, verify for floating parts and weak joints.";

struct Server {
    user: String,
    lib: Option<Arc<Library>>,
    sessions: HashMap<PathBuf, Session>,
    current: Option<PathBuf>,
}

impl Server {
    fn lib(&mut self) -> Result<Arc<Library>, String> {
        if self.lib.is_none() {
            self.lib = Some(library()?);
        }
        Ok(self.lib.clone().unwrap())
    }

    fn open(&mut self, path: &Path, create: bool, import: Option<&Path>) -> Result<PathBuf, String> {
        if !path.exists() {
            if !create && import.is_none() {
                return Err(format!("{} does not exist; pass create:true to make it", path.display()));
            }
            let title = path.file_stem().and_then(|s| s.to_str()).unwrap_or("model").to_string();
            create_project(path, &title, import)?;
        }
        let key = path.canonicalize().map_err(|e| e.to_string())?;
        if !self.sessions.contains_key(&key) {
            let lib = self.lib()?;
            let s = Session::open(&key, &self.user, lib)?;
            self.sessions.insert(key.clone(), s);
        }
        self.current = Some(key.clone());
        Ok(key)
    }

    fn call(&mut self, tool: &str, mut args: Value) -> Reply {
        let result = (|| -> Result<Reply, String> {
            if !TOOLS.iter().any(|t| t.name == tool) {
                return Err(format!("unknown tool {tool:?}"));
            }
            let obj = args.as_object_mut();
            let project = obj.and_then(|o| o.remove("project")).and_then(|v| v.as_str().map(PathBuf::from));
            if tool == "project" && args.get("action").and_then(Value::as_str) == Some("open") {
                let path = args
                    .get("path")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .or(project)
                    .ok_or("open needs path")?;
                let create = args.get("create").and_then(Value::as_bool).unwrap_or(false);
                let import = args.get("import").and_then(Value::as_str).map(PathBuf::from);
                let key = self.open(&path, create, import.as_deref())?;
                let s = self.sessions.get_mut(&key).unwrap();
                return Ok(s.call("project", &json!({"action": "info"})));
            }
            if let Some(p) = project {
                self.open(&p, false, None)?;
            }
            let key = self
                .current
                .clone()
                .ok_or("no project is open: call project {action:\"open\", path:\"model.sb\", create:true}")?;
            let s = self.sessions.get_mut(&key).unwrap();
            Ok(s.call(tool, &args))
        })();
        result.unwrap_or_else(|e| Reply::fail(e, json!({})))
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
                    json!({"type": "string", "description": "Path of the .sb project (default: the one opened last)."}),
                );
            }
            json!({"name": t.name, "description": t.summary, "inputSchema": schema})
        })
        .collect();
    json!({ "tools": tools })
}

fn handle(server: &mut Server, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2025-06-18");
            let version = if ["2024-11-05", "2025-03-26", "2025-06-18"].contains(&asked) { asked } else { "2025-06-18" };
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
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
pub fn mcp(user: &str, project: Option<PathBuf>) -> Result<(), String> {
    if !crate::valid_user(user) {
        return Err(format!("invalid user name {user:?}"));
    }
    let mut server = Server { user: user.to_string(), lib: None, sessions: HashMap::new(), current: None };
    if let Some(p) = project {
        server.open(&p, false, None)?;
    }
    serve(&mut server, std::io::stdin().lock(), std::io::stdout().lock())
}

fn serve(server: &mut Server, input: impl BufRead, mut output: impl Write) -> Result<(), String> {
    for line in input.lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => {
                let out: Vec<Value> = batch.iter().filter_map(|m| handle(server, m)).collect();
                (!out.is_empty()).then(|| Value::Array(out))
            }
            Ok(msg) => handle(server, &msg),
            Err(e) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}})),
        };
        if let Some(r) = reply {
            writeln!(output, "{r}").map_err(|e| e.to_string())?;
            output.flush().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
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
        let mut server = Server { user: "claude".into(), lib: None, sessions: HashMap::new(), current: None };
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
        let mut out = Vec::new();
        serve(&mut server, input.as_bytes(), &mut out).unwrap();
        let lines: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 3, "the notification gets no reply");
        assert_eq!(lines[0]["result"]["serverInfo"]["name"], "superbrick");
        assert_eq!(lines[1]["result"]["tools"].as_array().unwrap().len(), TOOLS.len());
        assert_eq!(lines[2]["result"]["isError"], true);
    }
}
