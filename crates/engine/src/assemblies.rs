//! Clipboard assemblies are immutable flattened snapshots, not aliases of the source.
use super::*;

impl Snapshot {
    pub fn copied_assembly(&self) -> Result<Vec<PartSpec>> {
        let first = self
            .model
            .selection
            .first()
            .and_then(|id| self.model.parts.get(id))
            .ok_or("Select the arm/assembly first")?;
        if self.model.selection.iter().any(|id| {
            self.model
                .parts
                .get(id)
                .is_none_or(|p| p.file != first.file)
        }) {
            return Err("Copy pieces from one model panel at a time".into());
        }
        let lines: BTreeSet<_> = self
            .model
            .selection
            .iter()
            .map(|id| self.model.parts[id].line)
            .collect();
        let mut parts: Vec<_> = self
            .model
            .flat(first.file)
            .into_iter()
            .filter(|p| lines.contains(&p.root_line))
            .map(|p| PartSpec {
                name: p.name,
                color: p.color.0,
                pos: p.pos,
                rot: p.rot,
                id: None,
            })
            .collect();
        let anchor = parts.first().ok_or("Selection has no resolved pieces")?.pos;
        for p in &mut parts {
            p.pos -= anchor;
        }
        Ok(parts)
    }
    pub fn mirror_specs(
        &self,
        file: usize,
        parts: Vec<PartSpec>,
        axis: usize,
        pivot: DVec3,
    ) -> Result<Vec<PartSpec>> {
        let value = connectors::request(
            &self.model,
            file,
            json!({"action":"mirror_specs","specs":parts,"axis":axis,"pivot":pivot}),
        )?;
        serde_json::from_value(value["parts"].clone()).map_err(|e| e.to_string())
    }
    pub fn prepare_assembly(
        &self,
        lib: Arc<Library>,
        cache: Option<&mut placement::NativePlacementCache>,
        file: usize,
        mut parts: Vec<PartSpec>,
        near: DVec3,
        facing: DVec3,
        ground: bool,
        free: bool,
        rotation: DMat3,
    ) -> Result<PlacementSolution> {
        let anchor = parts.first().cloned().ok_or("Empty copied assembly")?;
        for p in &mut parts {
            p.id = None;
        }
        let mut e = self.fork(lib.clone());
        let value = e.apply_for_preview(Cmd::Add {
            file,
            parts,
            free: true,
        })?;
        let ids: Vec<Id> =
            serde_json::from_value(value["ids"].clone()).map_err(|e| e.to_string())?;
        let staged = e.snapshot();
        let mut solution = if let Some(cache) = cache {
            staged.prepare_native_placement(
                lib, cache, file, ids, anchor, near, facing, ground, free, rotation,
            )?
        } else {
            staged
                .prepare_placement(lib, file, ids, anchor, near, facing, ground, free, rotation)?
        };
        solution.prepared.rebase(self);
        Ok(solution)
    }
}
