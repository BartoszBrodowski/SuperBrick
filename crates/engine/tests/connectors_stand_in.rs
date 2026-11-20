//! The connector backend's transport, with stand-in helper modules instead of `bench-next`.
//!
//! The real physics/snaps helpers are not in this repository (and the published
//! release predates some of them), but everything the native side does with the helper is: find
//! Python, write the embedded `connectors.py` to the cache and start it from there (Windows caps a
//! `-c` command line at 32 KB), hide its console window, exchange JSON lines over its pipes in
//! UTF-8, and let it exit quietly when the parent goes away. These tests run that path for real
//! against tiny stand-ins that answer like the real modules, so they need only Python with NumPy.
//! Part names carry non-ASCII text both ways: a helper reading stdin in the Windows locale
//! encoding would hand back a garbled name.
use std::path::{Path, PathBuf};

/// `physics`: a body per part with one connector whose label echoes the part name.
const PHYSICS: &str = r#"
import numpy as np
def get_shape(name):
    return name
class Conn:
    def __init__(self, pos, name):
        self.pos = np.asarray(pos, float)
        self.axis = np.array([0.0, -1.0, 0.0])
        self._label = 'stand-in ' + name
    def label(self):
        return self._label
class Body:
    def __init__(self, shape, R, t, uid=None):
        self.uid = uid
        self.conns = [Conn(t, shape)]
        self.flo = np.asarray(t, float) - 10
        self.fhi = np.asarray(t, float) + 10
def snap_links(a, b):
    return [[]]
def relation(a, b):
    return None
"#;

/// `snaps`: what `cad_snap_rules.apply` wraps, plus the calls `endpoints`/`compatible` make.
const SNAPS: &str = r#"
def make_conns(kind, properties, src, rule="ldcad"):
    return []
def compatible(a, b):
    return True
def combine_dof(matches):
    return 'fixed', None
def mate(local, world, angle, flip):
    return []
"#;

/// Two parts, one with non-ASCII in its name.
const MODEL: &[u8] = "1 4 0 0 0 1 0 0 0 1 0 0 0 1 3005.dat\n1 1 100 0 0 1 0 0 0 1 0 0 0 1 brique-é✓.dat\n".as_bytes();

/// `<tmp>/connectors/{bench,shadow}` with the stand-ins and the repository's own policy modules.
fn stand_in_data() -> PathBuf {
    let root = std::env::temp_dir().join(format!("sb-connector-stand-in-{}", std::process::id()));
    let bench = root.join("bench");
    std::fs::create_dir_all(&bench).unwrap();
    std::fs::create_dir_all(root.join("shadow")).unwrap();
    let app = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/src");
    for (name, text) in [("physics.py", PHYSICS), ("snaps.py", SNAPS)] {
        std::fs::write(bench.join(name), text).unwrap();
    }
    for name in ["cad_contact_policy.py", "cad_snap_rules.py", "cad_validate.py", "cad_scene.py"] {
        std::fs::copy(app.join(name), bench.join(name)).unwrap();
    }
    root
}

/// Python with NumPy (what `python_command` needs), or `None` to skip.
fn python() -> Option<PathBuf> {
    let found = ldraw::runtime::python();
    if found.is_none() {
        eprintln!("skip: no Python with NumPy and Pillow");
    }
    found
}

/// One test, so nothing reads the environment while it is being set.
#[test]
fn connector_backend_transport() {
    let Some(python) = python() else { return };
    let data = stand_in_data();
    round_trip_through_the_native_service(&data);
    helper_exits_quietly_when_the_parent_closes_its_pipes(&python, &data);
    let _ = std::fs::remove_dir_all(&data);
}

fn round_trip_through_the_native_service(data: &Path) {
    let cache = data.join("cache");
    // SAFETY: the only test in this binary; no other thread reads the environment meanwhile.
    unsafe {
        std::env::set_var("SB_CONNECTOR_DATA", data);
        std::env::set_var("SB_CACHE_DIR", &cache);
    }
    let model = engine::Model::from_document(ldraw::Document::parse(MODEL, "stand-in.ldr"), None);

    let reply = engine::connectors::request(&model, 0, serde_json::json!({"action": "endpoints"})).unwrap();
    let analysis: engine::connectors::Analysis = serde_json::from_value(reply).unwrap();
    let labels: Vec<&str> = analysis.connectors.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["stand-in 3005.dat", "stand-in brique-é✓.dat"], "names must survive the pipe in UTF-8");
    assert_eq!(analysis.connectors[1].part, 2);

    // The same helper process answers again (it is kept running), including errors.
    let source = analysis.connectors[0].endpoint();
    let reply = engine::connectors::request(&model, 0, serde_json::json!({"action": "compatible", "source": source})).unwrap();
    assert_eq!(reply["destinations"][0]["part"], 2);
    let err = engine::connectors::request(&model, 0, serde_json::json!({"action": "no-such-action"})).unwrap_err();
    assert!(err.contains("Unknown connector action"), "{err}");

    // The helper was started from the cache, not as `python -c` source.
    let helpers: Vec<_> = std::fs::read_dir(cache.join("helpers")).unwrap().flatten().map(|e| e.file_name()).collect();
    assert!(helpers.iter().any(|n| n.to_string_lossy().starts_with("connectors-")), "{helpers:?}");
}

fn helper_exits_quietly_when_the_parent_closes_its_pipes(python: &Path, data: &Path) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let script = ldraw::runtime::helper_script("connectors", include_str!("../src/connectors.py")).unwrap();
    let mut command = Command::new(python);
    ldraw::platform::no_window(&mut command);
    let mut child = command
        .arg("-u")
        .arg(script)
        .arg(data.join("bench"))
        .arg(data.join("shadow"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all("{\"action\":\"endpoints\",\"parts\":[{\"id\":1,\"name\":\"brique-é✓.dat\",\"color\":4,\"pos\":[0,0,0],\"rot\":[1,0,0,0,1,0,0,0,1]}],\"roots\":[{\"id\":1}]}\n".as_bytes())
        .unwrap();
    drop(child.stdin.take());
    let result = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert!(stderr.trim().is_empty(), "no traceback expected: {stderr}");
}
