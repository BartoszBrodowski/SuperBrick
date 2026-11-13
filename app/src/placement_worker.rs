//! One running solve and one replaceable cursor request; never queues cursor history.
use engine::{PartSpec, PlacementSolution, Snapshot};
use glam::{DMat3, DVec3};
use std::sync::{Arc, Condvar, Mutex, mpsc};
pub struct Job {
    pub query: String,
    pub snapshot: Arc<Snapshot>,
    pub lib: Arc<ldraw::Library>,
    pub file: usize,
    pub ids: Vec<u64>,
    pub part: PartSpec,
    pub assembly: Vec<PartSpec>,
    pub near: DVec3,
    pub facing: DVec3,
    pub ground: bool,
    pub free: bool,
    pub rotation: DMat3,
}
pub struct Completion {
    pub query: String,
    pub result: engine::Result<PlacementSolution>,
    pub blockers: Vec<u64>,
}
#[derive(Default)]
struct State {
    job: Option<Job>,
    generation: u64,
    closed: bool,
}
pub struct Worker {
    state: Arc<(Mutex<State>, Condvar)>,
    rx: mpsc::Receiver<Completion>,
    ready: Option<Completion>,
}
impl Worker {
    pub fn new(ctx: egui::Context) -> Self {
        Self::with_solver(ctx, |job| {
            if !job.assembly.is_empty() {
                return job.snapshot.prepare_assembly(
                    job.lib.clone(),
                    None,
                    job.file,
                    job.assembly.clone(),
                    job.near,
                    job.facing,
                    job.ground,
                    job.free,
                    job.rotation,
                );
            }
            job.snapshot.prepare_placement(
                job.lib.clone(),
                job.file,
                job.ids.clone(),
                job.part.clone(),
                job.near,
                job.facing,
                job.ground,
                job.free,
                job.rotation,
            )
        })
    }
    pub(crate) fn with_solver(
        ctx: egui::Context,
        solve: impl Fn(&Job) -> engine::Result<PlacementSolution> + Send + 'static,
    ) -> Self {
        let state = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let copy = state.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            loop {
                let (job, generation) = {
                    let (lock, wake) = &*copy;
                    let mut s = lock.lock().unwrap();
                    while s.job.is_none() && !s.closed {
                        s = wake.wait(s).unwrap();
                    }
                    if s.closed {
                        return;
                    }
                    let generation = s.generation;
                    (s.job.take().unwrap(), generation)
                };
                let result = solve(&job);
                {
                    let s = copy.0.lock().unwrap();
                    if s.closed {
                        return;
                    }
                    if s.generation != generation {
                        continue;
                    }
                }
                let blockers = if result.is_err() {
                    job.snapshot.hidden_placement_blockers(
                        job.lib,
                        job.file,
                        &job.ids,
                        &job.part,
                        job.near,
                        job.rotation,
                        job.ground,
                    )
                } else {
                    vec![]
                };
                let s = copy.0.lock().unwrap();
                if s.closed {
                    return;
                }
                if s.generation == generation {
                    let _ = tx.send(Completion {
                        query: job.query,
                        result,
                        blockers,
                    });
                    ctx.request_repaint();
                }
            }
        });
        Self {
            state,
            rx,
            ready: None,
        }
    }
    pub fn submit(&mut self, job: Job) {
        let mut s = self.state.0.lock().unwrap();
        s.generation += 1;
        s.job = Some(job);
        self.state.1.notify_one();
    }
    pub fn poll(&mut self) -> Option<Completion> {
        let mut last = self.ready.take();
        while let Ok(r) = self.rx.try_recv() {
            last = Some(r);
        }
        last
    }
    #[cfg(test)]
    pub fn wait(&mut self) {
        if self.ready.is_none() {
            self.ready = self
                .rx
                .recv_timeout(std::time::Duration::from_secs(20))
                .ok();
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let mut s = self.state.0.lock().unwrap();
        s.closed = true;
        s.job = None;
        self.state.1.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slow_solver_does_not_block_submission_and_drops_cursor_history() {
        let (started, seen) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let mut worker = Worker::with_solver(egui::Context::default(), move |job| {
            started.send(job.query.clone()).unwrap();
            gate.recv().unwrap();
            Err(job.query.clone())
        });
        let lib = Arc::new(ldraw::Library::open_default().unwrap());
        let handle = engine::Handle::spawn(engine::Engine::new(
            lib.clone(),
            ldraw::Document::new("worker.ldr", "test"),
            None,
        ));
        let job = |name: &str| Job {
            assembly: vec![],
            query: name.into(),
            snapshot: handle.snapshot(),
            lib: lib.clone(),
            file: 0,
            ids: vec![],
            part: PartSpec {
                name: "3005.dat".into(),
                color: 4,
                pos: DVec3::ZERO,
                rot: DMat3::IDENTITY,
                id: None,
            },
            near: DVec3::ZERO,
            facing: DVec3::NEG_Y,
            ground: true,
            free: false,
            rotation: DMat3::IDENTITY,
        };
        worker.submit(job("old"));
        assert_eq!(
            seen.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            "old"
        );
        worker.submit(job("discarded"));
        worker.submit(job("newest"));
        handle.command(engine::Cmd::Select { ids: vec![] }).unwrap();
        assert!(worker.poll().is_none());
        release.send(()).unwrap();
        assert_eq!(
            seen.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            "newest"
        );
        release.send(()).unwrap();
        worker.wait();
        let result = worker.poll().unwrap();
        assert_eq!(result.query, "newest");
        assert!(result.result.is_err());
        assert!(seen.try_recv().is_err());
    }
}
