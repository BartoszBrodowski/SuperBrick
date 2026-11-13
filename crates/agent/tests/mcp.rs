use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    sync::Arc,
};
fn engine() -> engine::Handle {
    engine::Handle::spawn(engine::Engine::new(
        Arc::new(ldraw::Library::open_default().unwrap()),
        ldraw::Document::new("model.ldr", "test"),
        None,
    ))
}
#[test]
fn json_rpc_errors_notifications_and_registry_parity() {
    let h = engine();
    assert!(
        agent::dispatch(
            &h,
            "test",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .is_none()
    );
    let r = agent::dispatch(
        &h,
        "test",
        json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
    )
    .unwrap();
    assert_eq!(r["result"]["serverInfo"]["name"], "cad");
    let tools = agent::tools();
    for def in engine::registry() {
        assert!(tools.iter().any(|t| t["name"] == def.name));
    }
    let r=agent::dispatch(&h,"test",json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"not_a_tool","arguments":{}}})).unwrap();
    assert_eq!(r["result"]["isError"], true);
}
#[test]
fn real_socket_is_private_and_writes_are_proposals() {
    let h = engine();
    let path = std::env::temp_dir().join(format!("cad-mcp-test-{}.sock", std::process::id()));
    let server = agent::serve(h.clone(), path.clone()).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(agent::serve(h.clone(), path.clone()).is_err());
    let mut stream = UnixStream::connect(&path).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"add","arguments":{"file":0,"parts":[{"name":"3003.dat","pos":[0,0,0]}]}}});
    writeln!(stream, "{request}").unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let r: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(r["result"]["isError"], false);
    assert!(h.snapshot().model.parts.is_empty());
    assert_eq!(h.snapshot().drafts.len(), 1);
    let session = h.snapshot().drafts[0].session.clone();
    h.call(move |e| e.accept(&session, None)).unwrap();
    assert_eq!(h.snapshot().model.parts.len(), 1);
    drop(server);
    assert!(!path.exists());
}

#[test]
fn startup_recovers_stale_socket_but_preserves_live_windows_and_files() {
    use std::os::unix::net::UnixListener;
    let h = engine();
    let dir = std::env::temp_dir().join(format!("cad-window-test-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("default.sock");
    // Crash residue: a bound address with no remaining listener.
    drop(UnixListener::bind(&path).unwrap());
    let first = agent::serve_window(h.clone(), path.clone(), true).unwrap();
    assert_eq!(first.path, path);
    let second = agent::serve_window(h.clone(), path.clone(), true).unwrap();
    assert_ne!(second.path, path);
    assert!(UnixStream::connect(&first.path).is_ok());
    assert!(UnixStream::connect(&second.path).is_ok());
    assert!(agent::serve_window(h.clone(), path.clone(), false).is_err());
    drop(first);
    drop(second);
    std::fs::write(&path, "not a socket, do not delete").unwrap();
    assert!(agent::serve_window(h, path.clone(), false).is_err());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "not a socket, do not delete"
    );
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(dir).unwrap();
}
