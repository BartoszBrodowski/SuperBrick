//! Transitional full-connector backend. Native transactions remain the only writer.
use crate::{Model, Result, joints::JointGraph};
use glam::{DMat3, DVec3};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Stdio},
    sync::{Mutex, OnceLock},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Endpoint {
    pub part: u64,
    pub leaf: Option<usize>,
    pub index: Option<usize>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Connector {
    pub part: u64,
    pub leaf: usize,
    pub index: usize,
    pub position: DVec3,
    pub axis: DVec3,
    pub label: String,
}
impl Connector {
    pub fn endpoint(&self) -> Endpoint {
        Endpoint {
            part: self.part,
            leaf: Some(self.leaf),
            index: Some(self.index),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Analysis {
    pub graph: JointGraph,
    pub connectors: Vec<Connector>,
    #[serde(default)]
    pub near_joints: Vec<NearJoint>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NearJoint {
    pub source: Endpoint,
    pub target: Endpoint,
    pub distance: f64,
    pub position: DVec3,
    pub label: String,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Pose {
    pub id: u64,
    pub pos: DVec3,
    pub rot: DMat3,
}
struct Backend {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}
impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn start() -> Result<Backend> {
    let installed = ldraw::installed_share("connectors");
    let data = ldraw::app_env("CONNECTOR_DATA")
        .map(PathBuf::from)
        .or_else(|| installed.filter(|p| p.join("bench/snaps.py").is_file()));
    let (bench, shadow) = if let Some(data) = data {
        (data.join("bench"), data.join("shadow"))
    } else {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
        (
            root.join("bench-next"),
            root.join("data-next/ldcad_shadow/LDCadShadowLibrary-main"),
        )
    };
    let mut child = ldraw::runtime::python_command()?
        .arg("-u")
        .arg("-c")
        .arg(include_str!("connectors.py"))
        .arg(bench)
        .arg(shadow)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("Connector backend {}: {e}", ldraw::runtime::NEEDS_PYTHON))?;
    Ok(Backend {
        input: child.stdin.take().unwrap(),
        output: BufReader::new(child.stdout.take().unwrap()),
        child,
    })
}
pub fn request(model: &Model, file: usize, mut action: Value) -> Result<Value> {
    if file >= model.doc.files.len() {
        return Err("unknown model file".into());
    }
    let ids: std::collections::HashMap<_, _> = model
        .parts
        .values()
        .filter(|p| p.file == file)
        .map(|p| (p.line, p.id))
        .collect();
    let hidden: std::collections::HashSet<_> = model
        .parts
        .values()
        .filter(|p| p.hidden)
        .map(|p| (0, p.file, p.line))
        .collect();
    let parts: Vec<_> = model
        .flat(file)
        .iter()
        .map(|p| {let hidden=p.ancestors.iter().chain(std::iter::once(&p.src)).any(|s|hidden.contains(s));json!({"id":ids.get(&p.root_line),"name":p.name,"pos":p.pos,"rot":p.rot,"hidden":hidden})})
        .collect();
    action["parts"] = json!(parts);
    action["roots"] = json!(
        model
            .parts
            .values()
            .filter(|p| p.file == file)
            .map(|p| json!({"id":p.id,"pos":p.pos,"rot":p.rot}))
            .collect::<Vec<_>>()
    );
    // Exact request equality includes every source pose: never reuse validation after an edit.
    static DRAG_CACHE: OnceLock<Mutex<Option<(Value, Value)>>> = OnceLock::new();
    let cacheable = action["action"] == "drag";
    if cacheable {
        if let Some((key, value)) = DRAG_CACHE
            .get_or_init(|| Mutex::new(None))
            .lock()
            .map_err(|_| "drag cache poisoned")?
            .as_ref()
        {
            if key == &action {
                return Ok(value.clone());
            }
        }
    }
    static SERVICE: OnceLock<Mutex<Option<Backend>>> = OnceLock::new();
    // Interactive endpoint discovery must never queue behind a planner/drag solve.
    static PICK_SERVICE: OnceLock<Mutex<Option<Backend>>> = OnceLock::new();
    let service = if matches!(action["action"].as_str(), Some("endpoints" | "compatible")) {
        &PICK_SERVICE
    } else {
        &SERVICE
    };
    let mut slot = service
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "connector service lock poisoned")?;
    if slot.is_none() {
        *slot = Some(start()?);
    }
    let service = slot.as_mut().unwrap();
    let result = (|| -> Result<Value> {
        writeln!(service.input, "{action}").map_err(|e| e.to_string())?;
        let mut line = String::new();
        service
            .output
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        if line.is_empty() {
            return Err("Connector backend stopped. Check Python/NumPy (SB_PYTHON) and shadow library installation".into());
        }
        let value: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        if let Some(e) = value.get("error").and_then(Value::as_str) {
            return Err(e.into());
        }
        Ok(value["result"].clone())
    })();
    if service.child.try_wait().ok().flatten().is_some() {
        *slot = None;
    }
    if cacheable {
        if let Ok(value) = &result {
            *DRAG_CACHE
                .get_or_init(|| Mutex::new(None))
                .lock()
                .map_err(|_| "drag cache poisoned")? = Some((action, value.clone()));
        }
    }
    result
}
pub fn analyze(model: &Model, file: usize) -> Result<Analysis> {
    serde_json::from_value(request(model, file, json!({"action":"analyze"}))?)
        .map_err(|e| e.to_string())
}
