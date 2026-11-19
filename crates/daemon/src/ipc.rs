//! D-Bus service `org.wavectl.Daemon` on the session bus.

use std::sync::Arc;

use wavectl_core::{
    api::{BUS_NAME, OBJECT_PATH},
    Behavior,
};
use tokio::sync::mpsc::UnboundedReceiver;
use zbus::fdo;

use crate::{
    mpris::{self, MprisCmd},
    shared::{Cmd, Shared},
};

pub struct Iface {
    pub shared: Arc<Shared>,
}

#[zbus::interface(name = "org.wavectl.Daemon")]
impl Iface {
    /// StateView as JSON.
    async fn get_state(&self) -> String {
        serde_json::to_string(&*self.shared.state.lock().unwrap()).unwrap_or_default()
    }
    async fn set_priority(&self, order: Vec<String>) {
        self.shared.send(Cmd::SetPriority(order));
    }
    async fn set_depth(&self, source: String, db: f64) {
        self.shared.send(Cmd::SetDepth(source, Some(db as f32)));
    }
    async fn clear_depth(&self, source: String) {
        self.shared.send(Cmd::SetDepth(source, None));
    }
    /// `behavior`: "duck" | "pause" | "mute" | "nothing"; "" restores the default.
    async fn set_behavior(&self, source: String, behavior: String) -> fdo::Result<()> {
        let b = if behavior.is_empty() {
            None
        } else {
            Some(Behavior::parse(&behavior).ok_or_else(|| fdo::Error::InvalidArgs(format!("unknown behavior {behavior:?}")))?)
        };
        self.shared.send(Cmd::SetBehavior(source, b));
        Ok(())
    }
    async fn set_default_depth(&self, db: f64) {
        self.shared.send(Cmd::SetDefaultDepth(db as f32));
    }
    async fn set_enabled(&self, on: bool) {
        self.shared.send(Cmd::SetEnabled(on));
    }
    async fn remove_source(&self, source: String) {
        self.shared.send(Cmd::RemoveSource(source));
    }
    async fn set_stream_volume(&self, id: u32, volume: f64) {
        self.shared.send(Cmd::SetStreamVolume(id, volume as f32));
    }
    async fn set_stream_mute(&self, id: u32, mute: bool) {
        self.shared.send(Cmd::SetStreamMute(id, mute));
    }
    /// `sink` = 0 means follow the default output.
    async fn route_stream(&self, id: u32, sink: u32) {
        self.shared.send(Cmd::RouteStream(id, (sink != 0).then_some(sink)));
    }
    async fn send_stream(&self, id: u32, capture: u32, on: bool) {
        self.shared.send(Cmd::SendStream(id, capture, on));
    }
    /// Transport control for a media player: play | pause | playpause | next | previous.
    async fn player_control(&self, id: String, action: String) {
        let _ = self.shared.mpris.send(MprisCmd::Control { id, action });
    }
    async fn promote_stream(&self, id: u32) {
        self.shared.send(Cmd::PromoteStream(id));
    }
    async fn reload(&self) {
        self.shared.send(Cmd::Reload);
    }
}

/// Run the D-Bus service and the MPRIS task on a tokio runtime in this thread.
pub fn run(shared: Arc<Shared>, mpris_rx: UnboundedReceiver<MprisCmd>) {
    let players = shared.players.clone();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().worker_threads(2).build().expect("tokio");
    rt.block_on(async move {
        tokio::spawn(mpris::run(mpris_rx, players));
        let conn = zbus::connection::Builder::session()
            .and_then(|b| b.name(BUS_NAME))
            .and_then(|b| b.serve_at(OBJECT_PATH, Iface { shared }));
        match conn {
            Ok(b) => match b.build().await {
                Ok(_conn) => std::future::pending::<()>().await,
                Err(e) => eprintln!("D-Bus failed: {e} (is another wavectld running?)"),
            },
            Err(e) => eprintln!("D-Bus setup failed: {e}"),
        }
    });
}
