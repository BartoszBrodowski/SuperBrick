//! Local MCP transport. Agents propose edits; only explicit acceptance changes the user model.
use engine::{Cmd, Handle};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

pub fn socket_path() -> PathBuf {
    ldraw::app_env("SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("cad-native.sock"))
}
pub struct Server {
    pub path: PathBuf,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn serve(handle: Handle, path: PathBuf) -> Result<Server, String> {
    // Never unlink an existing socket: it may belong to another running CAD instance.
    let listener = UnixListener::bind(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    let counter = Arc::new(AtomicU64::new(1));
    thread::Builder::new()
        .name("sb-mcp".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let h = handle.clone();
                let session = format!("mcp-{}", counter.fetch_add(1, Ordering::Relaxed));
                thread::spawn(move || {
                    let _ = connection(stream, h, session);
                });
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(Server { path })
}

/// Desktop startup: reclaim only an owned, refused (not live) socket. An additional
/// window uses a separate address instead of stealing the first window's endpoint.
pub fn serve_window(
    handle: Handle,
    path: PathBuf,
    allow_alternate: bool,
) -> Result<Server, String> {
    if let Ok(server) = serve(handle.clone(), path.clone()) {
        return Ok(server);
    }
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        let owned = meta.uid() == unsafe { libc::geteuid() };
        let refused = UnixStream::connect(&path)
            .is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionRefused);
        if meta.file_type().is_socket() && owned && refused {
            // Check identity again before removing only the stale socket we inspected.
            if std::fs::symlink_metadata(&path)
                .is_ok_and(|m| m.ino() == meta.ino() && m.dev() == meta.dev())
            {
                std::fs::remove_file(&path).map_err(|e| e.to_string())?;
                if let Ok(server) = serve(handle.clone(), path.clone()) {
                    return Ok(server);
                }
            }
        }
    }
    if allow_alternate {
        static WINDOWS: AtomicU64 = AtomicU64::new(1);
        let alternative = path.with_file_name(format!(
            "cad-native-{}-{}.sock",
            std::process::id(),
            WINDOWS.fetch_add(1, Ordering::Relaxed)
        ));
        serve(handle, alternative)
    } else {
        serve(handle, path)
    }
}

fn connection(mut stream: UnixStream, handle: Handle, session: String) -> std::io::Result<()> {
    let mut read = BufReader::new(stream.try_clone()?);
    loop {
        let mut b = Vec::new();
        // Bound frames without buffering arbitrarily large local input.
        let n = std::io::Read::take(&mut read, 4 * 1024 * 1024 + 1).read_until(b'\n', &mut b)?;
        if n == 0 {
            break;
        }
        if b.len() > 4 * 1024 * 1024 {
            return Err(std::io::Error::other("MCP frame too large"));
        }
        let request = serde_json::from_slice::<Value>(&b);
        let response = match request {
            Ok(request) => dispatch(&handle, &session, request),
            Err(e) => Some(
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":e.to_string()}}),
            ),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut stream, &response)?;
            stream.write_all(b"\n")?;
            stream.flush()?;
        }
    }
    Ok(())
}
pub fn dispatch(handle: &Handle, session: &str, request: Value) -> Option<Value> {
    let id = request.get("id")?.clone();
    let method = request["method"].as_str().unwrap_or("");
    let result = match method {
        "initialize" => Ok(
            json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"cad","version":"0.1.0"},"instructions":"Edits are version-stamped proposals. Read summary/list_parts before editing user parts. The user accepts/rejects drafts in the CAD. Native validation is currently partial; UNVERIFIED is not a legal guarantee."}),
        ),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools":tools()})),
        "tools/call" => {
            let name = request["params"]["name"].as_str().unwrap_or("");
            let args = request["params"]["arguments"].clone();
            let response = call(handle, session, name, args);
            Ok(match response {
                Ok(v) if v.get("png_base64").is_some() => {
                    json!({"content":[{"type":"image","mimeType":"image/png","data":v["png_base64"]}],"isError":false})
                }
                Ok(v) => {
                    json!({"content":[{"type":"text","text":serde_json::to_string(&v).unwrap()}],"isError":false})
                }
                Err(e) => json!({"content":[{"type":"text","text":e}],"isError":true}),
            })
        }
        _ => Err(format!("unknown method {method}")),
    };
    Some(match result {
        Ok(v) => json!({"jsonrpc":"2.0","id":id,"result":v}),
        Err(e) => json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":e}}),
    })
}
pub fn tools() -> Vec<Value> {
    let mut out:Vec<_>=engine::registry().into_iter().map(|d| {
        let mut schema=d.schema;
        schema["properties"]["turn"]=json!({"type":"integer","minimum":0});
        json!({"name":d.name,"description":format!("Shared CAD command {}. Model edits are proposals, not direct writes.",d.name),"inputSchema":schema})
    }).collect();
    for (name, properties) in [
        ("summary", json!({})),
        ("list_parts", json!({"file":{"type":"integer"}})),
        ("get_selection", json!({})),
        ("check", json!({"file":{"type":"integer"}})),
        ("draft_status", json!({})),
        (
            "reject",
            json!({"ids":{"type":"array","items":{"type":"integer"}}}),
        ),
        (
            "search_parts",
            json!({"query":{"type":"string"},"color":{"type":"integer"},"limit":{"type":"integer","minimum":1,"maximum":100}}),
        ),
        ("colours", json!({})),
        ("changes_since", json!({"revision":{"type":"integer"}})),
        (
            "look",
            json!({"file":{"type":"integer","minimum":0},"width":{"type":"integer","minimum":64,"maximum":2048},"height":{"type":"integer","minimum":64,"maximum":2048}}),
        ),
    ] {
        out.push(json!({"name":name,"description":name,"inputSchema":{"type":"object","properties":properties,"additionalProperties":false}}));
    }
    out
}
pub fn call(handle: &Handle, session: &str, name: &str, mut args: Value) -> engine::Result<Value> {
    let session = session.to_string();
    let name = name.to_string();
    if name == "look" {
        use base64::Engine;
        let file = args["file"].as_u64().unwrap_or(0) as usize;
        let snap = handle.snapshot();
        if file >= snap.model.doc.files.len() {
            return Err("unknown model".into());
        }
        let lib = handle.call(|e| Ok(e.lib.clone()))?;
        let flat = snap.effective(lib.clone(), &session)?.flat(file);
        let scene = render::Scene::build(&lib, &flat, &Default::default());
        let gpu = render::Gpu::headless()?;
        let width = args["width"].as_u64().unwrap_or(1024).clamp(64, 2048) as u32;
        let height = args["height"].as_u64().unwrap_or(768).clamp(64, 2048) as u32;
        let cam =
            render::Camera::framing(scene.bounds.0, scene.bounds.1, width as f32 / height as f32);
        let r = render::Renderer::new(&gpu, render::COLOR_FORMAT, 4, &scene);
        let target = render::Offscreen::new(&gpu, 4, width, height);
        target.render(&gpu, &r, &cam);
        let rgba = target.read_rgba(&gpu);
        let mut bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut bytes, width, height);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header()
                .and_then(|mut w| w.write_image_data(&rgba))
                .map_err(|e| e.to_string())?;
        }
        return Ok(json!({"png_base64":base64::engine::general_purpose::STANDARD.encode(bytes)}));
    }
    if name == "search_parts" {
        static SEARCH: std::sync::OnceLock<Result<search::Searcher, String>> =
            std::sync::OnceLock::new();
        let search = SEARCH
            .get_or_init(|| {
                search::load(&search::data_dir(), true)
                    .or_else(|_| search::load(&search::data_dir(), false))
                    .map_err(|e| e.to_string())
            })
            .as_ref()
            .map_err(Clone::clone)?;
        let filters = search::Filters {
            color: args["color"].as_i64().map(|c| c as i32),
            ..Default::default()
        };
        let (hits, info) = search.filtered(
            args["query"].as_str().unwrap_or(""),
            &filters,
            args["limit"].as_u64().unwrap_or(20).min(100) as usize,
        );
        return Ok(
            json!({"parts":hits.iter().map(|h|&search.cat.parts[h.idx]).collect::<Vec<_>>(),"query_ms":info.total_us/1000.0}),
        );
    }
    handle.call(move |e| {
        let notices:Vec<_>=e.drafts.ops.iter().filter(|o|o.session==session && ["stale","blocked"].contains(&o.state.as_str())).cloned().collect();
        let value=match name.as_str() {
            "summary"=>{
                e.observe(&session);
                json!({"revision":e.revision,"parts":e.model.parts.len(),"files":e.model.doc.files.iter().map(|f|&f.name).collect::<Vec<_>>(),"selection":e.model.selection,"session":session,"validation":"partial native port"})
            },
            "list_parts"=>{
                e.observe(&session);
                let effective=e.snapshot().effective(e.lib.clone(),&session)?;
                json!({"parts":effective.parts.values().filter(|p|args["file"].as_u64().is_none_or(|f|p.file==f as usize)).collect::<Vec<_>>()})
            },
            "get_selection"=>json!({"ids":e.model.selection}),
            "colours"=>json!({"colours":e.lib.colors.by_code.iter().map(|(code,c)|json!({"code":code,"name":c.ldraw_name})).collect::<Vec<_>>()}),
            "check"=>{let file=args["file"].as_u64().unwrap_or(0)as usize;if file>=e.model.doc.files.len(){return Err("unknown model".into());}let flat=e.snapshot().effective(e.lib.clone(),&session)?.flat(file);serde_json::to_value(e.validator.check(&e.lib,&flat)).map_err(|e|e.to_string())? },
            "draft_status"=>json!({"drafts":e.drafts.ops.iter().filter(|o|o.session==session).collect::<Vec<_>>()}),
            "reject"=>{e.reject(&session,serde_json::from_value(args["ids"].clone()).map_err(|e|e.to_string())?)?;json!({"rejected":true})},
            "changes_since"=>json!({"revision":e.revision,"parts":e.model.parts.values().filter(|p|p.revision>args["revision"].as_u64().unwrap_or(0)).collect::<Vec<_>>(),"notice":"Removed ids are reflected by list_parts; undo changes all surviving revisions."}),
            _=>{
                if !engine::registry().iter().any(|d|d.name==name) {return Err(format!("unknown tool {name}"));}
                let turn=args.get("turn").and_then(Value::as_u64).unwrap_or(0);
                args.as_object_mut().ok_or("arguments must be an object")?.remove("turn");
                args["command"]=json!(name);
                let cmd:Cmd=serde_json::from_value(args).map_err(|e|e.to_string())?;
                if matches!(cmd,Cmd::Select{..}|Cmd::Hide{..}|Cmd::Lock{..}) {e.execute(cmd)?}else{e.propose(session,turn,cmd)?}
            }
        };
        Ok(json!({"result":value,"notices":notices}))
    })
}

/// Newline-framed stdio shim for Claude Code and other MCP clients.
pub fn stdio(path: &Path) -> Result<(), String> {
    let stream = UnixStream::connect(path)
        .map_err(|e| format!("open CAD first; {}: {e}", path.display()))?;
    let mut to_socket = stream.try_clone().map_err(|e| e.to_string())?;
    let input = thread::spawn(move || {
        let r = std::io::copy(&mut std::io::stdin().lock(), &mut to_socket);
        let _ = to_socket.shutdown(std::net::Shutdown::Write);
        r
    });
    let mut reader = stream;
    std::io::copy(&mut reader, &mut std::io::stdout().lock()).map_err(|e| e.to_string())?;
    let _ = input.join();
    Ok(())
}
