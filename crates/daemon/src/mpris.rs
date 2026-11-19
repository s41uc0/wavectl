//! Media players: MPRIS players on the session bus (browsers, music apps) and
//! BlueZ media players (a phone over AVRCP). Used for the "pause" behavior, for
//! now-playing info, and for transport controls.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::sync::mpsc::UnboundedReceiver;
use zbus::{fdo, names::BusName, proxy::CacheProperties, zvariant::OwnedValue, Connection, Proxy};

/// What we know about a stream, used to find its media player.
#[derive(Debug, Clone, Default)]
pub struct Hint {
    /// Lowercased app name / binary / app id.
    pub names: Vec<String>,
    pub pid: Option<u32>,
    /// Bluetooth address of a phone/console streaming to us.
    pub bluez: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PlayerInfo {
    /// MPRIS bus name, or BlueZ object path (starts with '/').
    pub id: String,
    /// Display name, e.g. "Spotify".
    pub name: String,
    /// Lowercased strings to match a stream's app name against.
    pub cands: Vec<String>,
    pub pid: Option<u32>,
    pub bluez: Option<String>,
    /// "playing" | "paused" | "stopped"
    pub status: String,
    pub title: String,
    pub artist: String,
    pub album: String,
}

pub type Players = Arc<Mutex<Vec<PlayerInfo>>>;

#[derive(Debug)]
pub enum MprisCmd {
    /// Pause whatever is playing that belongs to these streams.
    Pause { source: String, hints: Vec<Hint> },
    /// Resume what we paused for this source.
    Resume { source: String },
    /// Transport control: play | pause | playpause | next | previous.
    Control { id: String, action: String },
}

/// A hint matches a player if any hint name and any candidate overlap
/// (either contains the other). Names shorter than 3 chars are ignored.
pub fn name_matches(hint_names: &[String], candidates: &[String]) -> bool {
    hint_names.iter().filter(|h| h.len() >= 3).any(|h| {
        candidates.iter().filter(|c| c.len() >= 3).any(|c| c.contains(h.as_str()) || h.contains(c.as_str()))
    })
}

/// Does this player belong to the stream described by `hint`?
pub fn player_matches(p: &PlayerInfo, hint: &Hint) -> bool {
    match (&p.bluez, &hint.bluez) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        (Some(_), None) | (None, Some(_)) => false,
        (None, None) => name_matches(&hint.names, &p.cands) || (hint.pid.is_some() && hint.pid == p.pid),
    }
}

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const BLUEZ_PLAYER: &str = "org.bluez.MediaPlayer1";

async fn proxy<'a>(conn: &'a Connection, dest: &'a str, path: &'a str, iface: &'a str) -> zbus::Result<Proxy<'a>> {
    zbus::proxy::Builder::<Proxy>::new(conn).destination(dest)?.path(path)?.interface(iface)?.cache_properties(CacheProperties::No).build().await
}

fn s(v: Option<&OwnedValue>) -> String {
    v.and_then(|v| String::try_from(v.clone()).ok()).unwrap_or_default()
}

fn artists(v: Option<&OwnedValue>) -> String {
    v.and_then(|v| {
        // MPRIS: array of strings; BlueZ: a plain string.
        Vec::<String>::try_from(v.clone()).ok().map(|a| a.join(", ")).or_else(|| String::try_from(v.clone()).ok())
    })
    .unwrap_or_default()
}

async fn collect_mpris(session: &Connection, out: &mut Vec<PlayerInfo>) {
    let Ok(dbus) = fdo::DBusProxy::new(session).await else { return };
    let Ok(names) = dbus.list_names().await else { return };
    for name in names {
        let n = name.as_str();
        let Some(suffix) = n.strip_prefix(MPRIS_PREFIX) else { continue };
        let Ok(player) = proxy(session, n, "/org/mpris/MediaPlayer2", "org.mpris.MediaPlayer2.Player").await else { continue };
        let Ok(status) = player.get_property::<String>("PlaybackStatus").await else { continue };
        let meta: HashMap<String, OwnedValue> = player.get_property("Metadata").await.unwrap_or_default();
        let (mut identity, mut desktop) = (String::new(), String::new());
        if let Ok(root) = proxy(session, n, "/org/mpris/MediaPlayer2", "org.mpris.MediaPlayer2").await {
            identity = root.get_property("Identity").await.unwrap_or_default();
            desktop = root.get_property("DesktopEntry").await.unwrap_or_default();
        }
        // firefox.instance_1_234 -> firefox
        let base = suffix.split(".instance").next().unwrap_or(suffix).to_lowercase();
        let pid = match BusName::try_from(n) {
            Ok(b) => dbus.get_connection_unix_process_id(b).await.ok(),
            Err(_) => None,
        };
        out.push(PlayerInfo {
            id: n.to_owned(),
            name: if identity.is_empty() { base.clone() } else { identity.clone() },
            cands: vec![base, identity.to_lowercase(), desktop.to_lowercase()],
            pid,
            bluez: None,
            status: status.to_lowercase(),
            title: s(meta.get("xesam:title")),
            artist: artists(meta.get("xesam:artist")),
            album: s(meta.get("xesam:album")),
        });
    }
}

async fn collect_bluez(system: &Connection, out: &mut Vec<PlayerInfo>) {
    let om = match fdo::ObjectManagerProxy::builder(system).destination("org.bluez").and_then(|b| b.path("/")) {
        Ok(b) => b.build().await,
        Err(e) => Err(e),
    };
    let Ok(om) = om else { return };
    let Ok(objs) = om.get_managed_objects().await else { return };
    for (path, ifaces) in objs {
        let Some(props) = ifaces.iter().find(|(i, _)| i.as_str() == BLUEZ_PLAYER).map(|(_, p)| p) else { continue };
        let track: HashMap<String, OwnedValue> = props.get("Track").and_then(|t| HashMap::try_from(t.clone()).ok()).unwrap_or_default();
        // /org/bluez/hci0/dev_80_B9_89_73_58_B8/avrcp/player0 -> 80:B9:89:73:58:B8
        let addr = path.as_str().split("/dev_").nth(1).and_then(|r| r.split('/').next()).map(|a| a.replace('_', ":"));
        let name = s(props.get("Name"));
        out.push(PlayerInfo {
            id: path.to_string(),
            cands: vec![name.to_lowercase()],
            name,
            pid: None,
            bluez: addr,
            status: s(props.get("Status")).to_lowercase(),
            title: s(track.get("Title")),
            artist: artists(track.get("Artist")),
            album: s(track.get("Album")),
        });
    }
}

async fn collect(session: &Connection, system: Option<&Connection>) -> Vec<PlayerInfo> {
    let mut out = vec![];
    collect_mpris(session, &mut out).await;
    if let Some(sys) = system {
        collect_bluez(sys, &mut out).await;
    }
    out
}

/// Send a transport command to one player.
async fn control(session: &Connection, system: Option<&Connection>, id: &str, action: &str) -> Result<(), String> {
    let e = |e: zbus::Error| e.to_string();
    if id.starts_with('/') {
        let sys = system.ok_or("no system bus")?;
        let p = proxy(sys, "org.bluez", id, BLUEZ_PLAYER).await.map_err(e)?;
        let method = match action {
            "play" => "Play",
            "pause" => "Pause",
            "next" => "Next",
            "previous" => "Previous",
            "playpause" => {
                if p.get_property::<String>("Status").await.unwrap_or_default() == "playing" { "Pause" } else { "Play" }
            }
            other => return Err(format!("unknown action {other:?}")),
        };
        p.call_method(method, &()).await.map_err(e)?;
    } else {
        let p = proxy(session, id, "/org/mpris/MediaPlayer2", "org.mpris.MediaPlayer2.Player").await.map_err(e)?;
        let method = match action {
            "play" => "Play",
            "pause" => "Pause",
            "playpause" => "PlayPause",
            "next" => "Next",
            "previous" => "Previous",
            other => return Err(format!("unknown action {other:?}")),
        };
        p.call_method(method, &()).await.map_err(e)?;
    }
    Ok(())
}

pub async fn run(mut rx: UnboundedReceiver<MprisCmd>, players: Players) {
    let session = match Connection::session().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mpris disabled: {e}");
            return;
        }
    };
    let system = Connection::system().await.ok();
    // Player ids we paused, per source, so we only resume our own pauses.
    let mut paused: HashMap<String, Vec<String>> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_millis(1000));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                *players.lock().unwrap() = collect(&session, system.as_ref()).await;
            }
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    MprisCmd::Pause { source, hints } => {
                        if paused.contains_key(&source) {
                            continue;
                        }
                        let infos = collect(&session, system.as_ref()).await;
                        let mut ids = vec![];
                        for p in infos.iter().filter(|p| p.status == "playing" && hints.iter().any(|h| player_matches(p, h))) {
                            match control(&session, system.as_ref(), &p.id, "pause").await {
                                Ok(()) => {
                                    eprintln!("paused {} ({})", p.name, p.id);
                                    ids.push(p.id.clone());
                                }
                                Err(e) => eprintln!("pause {} failed: {e}", p.id),
                            }
                        }
                        paused.insert(source, ids);
                        *players.lock().unwrap() = collect(&session, system.as_ref()).await;
                    }
                    MprisCmd::Resume { source } => {
                        let Some(ids) = paused.remove(&source) else { continue };
                        let infos = collect(&session, system.as_ref()).await;
                        for id in ids {
                            // Only resume if the user didn't change it meanwhile.
                            if infos.iter().any(|p| p.id == id && p.status == "paused") {
                                match control(&session, system.as_ref(), &id, "play").await {
                                    Ok(()) => eprintln!("resumed {id}"),
                                    Err(e) => eprintln!("resume {id} failed: {e}"),
                                }
                            }
                        }
                        *players.lock().unwrap() = collect(&session, system.as_ref()).await;
                    }
                    MprisCmd::Control { id, action } => {
                        if let Err(e) = control(&session, system.as_ref(), &id, &action).await {
                            eprintln!("control {id} {action}: {e}");
                        }
                        *players.lock().unwrap() = collect(&session, system.as_ref()).await;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: &[&str]) -> Vec<String> {
        x.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn matching() {
        assert!(name_matches(&v(&["firefox"]), &v(&["firefox", "mozilla firefox", ""])));
        assert!(name_matches(&v(&["spotify"]), &v(&["spotify"])));
        assert!(name_matches(&v(&["mpv media player"]), &v(&["mpv"])));
        assert!(!name_matches(&v(&["firefox"]), &v(&["spotify"])));
        // tiny names never match everything
        assert!(!name_matches(&v(&["a"]), &v(&["spotify"])));
        assert!(!name_matches(&v(&[]), &v(&["spotify"])));
    }

    #[test]
    fn players_match_their_stream() {
        let phone = PlayerInfo { bluez: Some("80:B9:89:73:58:B8".into()), cands: v(&["spotify"]), ..Default::default() };
        let desktop = PlayerInfo { cands: v(&["spotify"]), ..Default::default() };
        let phone_hint = Hint { bluez: Some("80:b9:89:73:58:b8".into()), names: v(&["i use arch btw"]), ..Default::default() };
        let app_hint = Hint { names: v(&["spotify"]), ..Default::default() };
        assert!(player_matches(&phone, &phone_hint));
        // A phone's player is never matched to a desktop app by name, and vice versa.
        assert!(!player_matches(&phone, &app_hint));
        assert!(!player_matches(&desktop, &phone_hint));
        assert!(player_matches(&desktop, &app_hint));
    }
}
