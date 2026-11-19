use std::sync::{mpsc, Arc, Mutex};

use wavectl_core::api::StateView;
use wavectl_core::Behavior;
use tokio::sync::mpsc::UnboundedSender;

use crate::mpris::{MprisCmd, Players};

/// Requests from D-Bus handlers to the PipeWire thread.
#[derive(Debug)]
pub enum Cmd {
    SetPriority(Vec<String>),
    SetDepth(String, Option<f32>),
    SetBehavior(String, Option<Behavior>),
    SetDefaultDepth(f32),
    SetEnabled(bool),
    RemoveSource(String),
    Reload,
    SetStreamVolume(u32, f32),
    SetStreamMute(u32, bool),
    /// Route a stream to an output device (None = follow the default).
    RouteStream(u32, Option<u32>),
    /// Link/unlink a stream into another program's recording input.
    SendStream(u32, u32, bool),
    /// Give a stream's program its own priority tier.
    PromoteStream(u32),
}

pub struct Shared {
    pub state: Mutex<StateView>,
    pub tx: Mutex<mpsc::Sender<Cmd>>,
    /// Pause/resume requests for the MPRIS task.
    pub mpris: UnboundedSender<MprisCmd>,
    /// Latest snapshot of media players (refreshed ~1 Hz by the MPRIS task).
    pub players: Players,
}

impl Shared {
    pub fn new(tx: mpsc::Sender<Cmd>, mpris: UnboundedSender<MprisCmd>) -> Arc<Self> {
        Arc::new(Self { state: Mutex::new(StateView::default()), tx: Mutex::new(tx), mpris, players: Default::default() })
    }

    pub fn send(&self, c: Cmd) {
        let _ = self.tx.lock().unwrap().send(c);
    }
}
