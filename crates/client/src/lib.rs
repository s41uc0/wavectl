//! Blocking D-Bus client for wavectld, shared by the CLI, GUI and tray.

use wavectl_core::api::StateView;
use zbus::blocking::Connection;

#[zbus::proxy(
    interface = "org.wavectl.Daemon",
    default_service = "org.wavectl.Daemon",
    default_path = "/org/wavectl/Daemon",
    gen_async = false
)]
trait Daemon {
    fn get_state(&self) -> zbus::Result<String>;
    fn set_priority(&self, order: Vec<String>) -> zbus::Result<()>;
    fn set_depth(&self, source: &str, db: f64) -> zbus::Result<()>;
    fn clear_depth(&self, source: &str) -> zbus::Result<()>;
    fn set_behavior(&self, source: &str, behavior: &str) -> zbus::Result<()>;
    fn set_default_depth(&self, db: f64) -> zbus::Result<()>;
    fn set_enabled(&self, on: bool) -> zbus::Result<()>;
    fn remove_source(&self, source: &str) -> zbus::Result<()>;
    fn set_stream_volume(&self, id: u32, volume: f64) -> zbus::Result<()>;
    fn set_stream_mute(&self, id: u32, mute: bool) -> zbus::Result<()>;
    fn route_stream(&self, id: u32, sink: u32) -> zbus::Result<()>;
    fn send_stream(&self, id: u32, capture: u32, on: bool) -> zbus::Result<()>;
    fn promote_stream(&self, id: u32) -> zbus::Result<()>;
    fn player_control(&self, id: &str, action: &str) -> zbus::Result<()>;
    fn reload(&self) -> zbus::Result<()>;
}

pub struct Client {
    proxy: DaemonProxy<'static>,
}

pub type Result<T> = std::result::Result<T, String>;

fn e(err: zbus::Error) -> String {
    match err {
        zbus::Error::MethodError(_, Some(msg), _) => msg,
        zbus::Error::MethodError(name, None, _) if name.contains("ServiceUnknown") => "wavectld is not running".into(),
        other => {
            let s = other.to_string();
            if s.contains("ServiceUnknown") || s.contains("not provided by any .service") {
                "wavectld is not running".into()
            } else {
                s
            }
        }
    }
}

impl Client {
    pub fn connect() -> Result<Self> {
        let conn = Connection::session().map_err(e)?;
        Ok(Self { proxy: DaemonProxy::new(&conn).map_err(e)? })
    }

    pub fn state(&self) -> Result<StateView> {
        serde_json::from_str(&self.proxy.get_state().map_err(e)?).map_err(|x| x.to_string())
    }
    pub fn set_priority(&self, order: Vec<String>) -> Result<()> { self.proxy.set_priority(order).map_err(e) }
    pub fn set_depth(&self, s: &str, db: f64) -> Result<()> { self.proxy.set_depth(s, db).map_err(e) }
    pub fn set_behavior(&self, s: &str, b: &str) -> Result<()> { self.proxy.set_behavior(s, b).map_err(e) }
    pub fn clear_depth(&self, s: &str) -> Result<()> { self.proxy.clear_depth(s).map_err(e) }
    pub fn set_default_depth(&self, db: f64) -> Result<()> { self.proxy.set_default_depth(db).map_err(e) }
    pub fn set_enabled(&self, on: bool) -> Result<()> { self.proxy.set_enabled(on).map_err(e) }
    pub fn remove_source(&self, s: &str) -> Result<()> { self.proxy.remove_source(s).map_err(e) }
    pub fn set_stream_volume(&self, id: u32, v: f64) -> Result<()> { self.proxy.set_stream_volume(id, v).map_err(e) }
    pub fn set_stream_mute(&self, id: u32, m: bool) -> Result<()> { self.proxy.set_stream_mute(id, m).map_err(e) }
    pub fn route_stream(&self, id: u32, sink: u32) -> Result<()> { self.proxy.route_stream(id, sink).map_err(e) }
    pub fn send_stream(&self, id: u32, capture: u32, on: bool) -> Result<()> { self.proxy.send_stream(id, capture, on).map_err(e) }
    pub fn player_control(&self, id: &str, action: &str) -> Result<()> { self.proxy.player_control(id, action).map_err(e) }
    pub fn promote_stream(&self, id: u32) -> Result<()> { self.proxy.promote_stream(id).map_err(e) }
    pub fn reload(&self) -> Result<()> { self.proxy.reload().map_err(e) }
}
