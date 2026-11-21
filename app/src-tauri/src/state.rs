use std::process::Child;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct Procs {
    pub agent: Option<Child>,
    pub syncthing: Option<Child>,
}

impl Procs {
    pub fn kill_all(&mut self) {
        for c in [self.agent.as_mut(), self.syncthing.as_mut()].into_iter().flatten() {
            let _ = c.kill();
        }
    }
}

#[derive(Clone, Default)]
pub struct AppState {
    pub procs: Arc<Mutex<Procs>>,
}
