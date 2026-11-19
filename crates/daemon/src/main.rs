//! wavectld: a PipeWire audio manager.
//!
//! - meters every program/device stream per channel
//! - ducks lower-priority sources via the node `softVolumes` prop (independent
//!   of the user's `channelVolumes` and the app's `volume`)
//! - per-stream volume/mute, routing to outputs, and linking one program's
//!   audio into another program's input

mod ipc;
mod mpris;
mod shared;

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use wavectl_core::{
    api::{CaptureView, OutputView, PlayerView, SourceView, StateView, StreamView},
    Behavior, Config, Engine, MatchRule, NodeInfo, SourceState,
};
use pipewire as pw;
use pw::{
    core::CoreRc,
    link::Link,
    metadata::{Metadata, MetadataListener},
    node::{Node, NodeListener},
    properties::{properties, PropertiesBox},
    registry::{GlobalObject, RegistryRc},
    spa::{
        self,
        pod::{deserialize::PodDeserializer, serialize::PodSerializer, Object, Pod, Property, Value, ValueArray},
        utils::{dict::DictRef, Direction},
    },
    stream::{StreamBox, StreamFlags, StreamListener},
    types::ObjectType,
};
use mpris::{player_matches, Hint, MprisCmd};
use shared::{Cmd, Shared};

type Props = Rc<RefCell<HashMap<String, String>>>;

const CLASS_PLAYBACK: &str = "Stream/Output/Audio";
const CLASS_CAPTURE: &str = "Stream/Input/Audio";
const CLASS_SINK: &str = "Audio/Sink";

/// The user-facing volume state of a node, as last reported by PipeWire.
#[derive(Default, Clone)]
struct Vol {
    channels: Vec<f32>,
    mute: bool,
}

impl Vol {
    fn level(&self) -> f32 {
        if self.channels.is_empty() { 1.0 } else { self.channels.iter().sum::<f32>() / self.channels.len() as f32 }
    }
}

/// A playback stream we meter, duck and control.
struct Tracked {
    /// Registry props merged with later `info` updates (apps set media.name late).
    props: Props,
    vol: Rc<RefCell<Vol>>,
    /// Smoothed per-channel display levels (dBFS).
    disp: (f32, f32),
    /// Detection level (dBFS): instant attack, fast decay, so bursty audio
    /// callbacks (e.g. 100 ms buffers) don't read as silence between bursts.
    det: f32,
    source: Option<String>,
    node: Node,
    _node_listener: NodeListener,
    _meter: StreamBox<'static>,
    _meter_listener: StreamListener<()>,
    /// Peak (linear) per channel since last tick.
    peaks: Rc<Cell<(f32, f32)>>,
    last_gain_db: f32,
}

impl Tracked {
    fn prop(&self, k: &str) -> String {
        self.props.borrow().get(k).cloned().unwrap_or_default()
    }

    fn node_info(&self) -> NodeInfo {
        let p = self.props.borrow();
        NodeInfo {
            node_name: p.get("node.name").cloned().unwrap_or_default(),
            bluez_address: p.get("api.bluez5.address").cloned(),
            is_app_stream: p.get("media.class").is_some_and(|c| c == CLASS_PLAYBACK),
            props: p.clone(),
        }
    }

    fn hint(&self) -> Hint {
        Hint {
            names: ["application.name", "application.process.binary", "application.id", "node.description"]
                .iter()
                .map(|k| self.prop(k).to_lowercase())
                .filter(|v| !v.is_empty())
                .collect(),
            pid: self.prop("application.process.id").parse().ok(),
            bluez: Some(self.prop("api.bluez5.address")).filter(|a| !a.is_empty()),
        }
    }

    fn label(&self) -> String {
        let p = self.props.borrow();
        ["device.alias", "node.description", "application.name", "node.name"]
            .iter()
            .find_map(|k| p.get(*k).filter(|v| !v.is_empty()))
            .cloned()
            .unwrap_or_else(|| "Unknown".into())
    }
}

/// Any other audio node we only need to describe (outputs, recording programs).
struct GNode {
    props: Props,
    _node: Node,
    _listener: NodeListener,
}

struct PortInfo {
    node: u32,
    out: bool,
    channel: String,
    monitor: bool,
}

struct LinkInfo {
    out_node: u32,
    in_node: u32,
}

struct State {
    core: CoreRc,
    registry: RegistryRc,
    engine: Engine,
    nodes: HashMap<u32, Tracked>,
    others: HashMap<u32, GNode>,
    ports: HashMap<u32, PortInfo>,
    links: HashMap<u32, LinkInfo>,
    default_sink: Rc<RefCell<Option<String>>>,
    metadata: Option<(Metadata, MetadataListener)>,
    dirty: Rc<Cell<bool>>,
    /// Sources whose players we paused (pause behavior), so we can resume them.
    paused: std::collections::HashSet<String>,
}

fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("wavectl/config.toml")
}

fn save_config(c: &Config) {
    let p = config_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    match c.to_toml() {
        Ok(s) => {
            if let Err(e) = std::fs::write(&p, s) {
                eprintln!("cannot write {}: {e}", p.display());
            }
        }
        Err(e) => eprintln!("cannot serialize config: {e}"),
    }
}

fn load_config() -> Result<Config> {
    let p = config_path();
    match std::fs::read_to_string(&p) {
        Ok(s) => Config::from_toml(&s).with_context(|| format!("parsing {}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let c = Config::default_hub();
            eprintln!("no config at {}, writing default", p.display());
            save_config(&c);
            Ok(c)
        }
        Err(e) => Err(e.into()),
    }
}

fn dict_to_map(d: &DictRef) -> HashMap<String, String> {
    d.iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect()
}

fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

fn parse_props(param: &Pod, vol: &mut Vol) {
    let Ok((_, Value::Object(obj))) = PodDeserializer::deserialize_any_from(param.as_bytes()) else { return };
    for p in obj.properties {
        match (p.key, p.value) {
            (spa::sys::SPA_PROP_channelVolumes, Value::ValueArray(ValueArray::Float(v))) => vol.channels = v,
            (spa::sys::SPA_PROP_mute, Value::Bool(b)) => vol.mute = b,
            _ => {}
        }
    }
}

fn set_props(node: &Node, props: Vec<Property>) {
    let obj = Object { type_: spa::sys::SPA_TYPE_OBJECT_Props, id: spa::sys::SPA_PARAM_Props, properties: props };
    if let Ok((cursor, _)) = PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj)) {
        let bytes = cursor.into_inner();
        if let Some(pod) = Pod::from_bytes(&bytes) {
            node.set_param(spa::param::ParamType::Props, 0, pod);
        }
    }
}

fn set_soft_volume(node: &Node, db: f32) {
    let lin = db_to_lin(db);
    set_props(node, vec![Property::new(spa::sys::SPA_PROP_softVolumes, Value::ValueArray(ValueArray::Float(vec![lin, lin])))]);
}

impl State {
    fn apply_config(&mut self, cfg: Config) {
        save_config(&cfg);
        self.engine.set_config(cfg);
        self.dirty.set(true);
    }

    /// Re-run source classification for every node (using up-to-date props);
    /// auto-add Bluetooth devices that have no source yet.
    fn reclassify(&mut self) {
        self.dirty.set(false);
        let mut cfg = self.engine.config().clone();
        let mut added = false;
        let ids: Vec<u32> = self.nodes.keys().copied().collect();
        for id in ids {
            let t = &self.nodes[&id];
            let info = t.node_info();
            let mut idx = cfg.classify(&info);
            if idx.is_none() {
                if let Some(addr) = info.bluez_address.clone() {
                    let rule = MatchRule { bluez_address: Some(addr), ..Default::default() };
                    let label = t.label();
                    idx = Some(cfg.add_source(&label, rule));
                    added = true;
                    eprintln!("auto-added source {label:?}");
                }
            }
            let name = idx.map(|i| cfg.sources[i].name.clone());
            self.nodes.get_mut(&id).unwrap().source = name;
        }
        if added {
            save_config(&cfg);
            self.engine.set_config(cfg);
        }
    }

    fn serial_of(&self, id: u32) -> Option<String> {
        let t = self.nodes.get(&id).map(|t| t.props.clone()).or_else(|| self.others.get(&id).map(|g| g.props.clone()))?;
        let s = t.borrow().get("object.serial").cloned();
        s
    }

    fn route(&mut self, stream: u32, sink: Option<u32>) {
        let Some((md, _)) = &self.metadata else {
            eprintln!("route: no default metadata object");
            return;
        };
        match sink.and_then(|s| self.serial_of(s).map(|ser| (s, ser))) {
            Some((sid, serial)) => {
                md.set_property(stream, "target.object", Some("Spa:Id"), Some(&serial));
                md.set_property(stream, "target.node", Some("Spa:Id"), Some(&sid.to_string()));
            }
            None => {
                md.set_property(stream, "target.object", None, None);
                md.set_property(stream, "target.node", None, None);
            }
        }
    }

    fn ports_of(&self, node: u32, out: bool) -> Vec<(u32, &PortInfo)> {
        let mut v: Vec<_> = self.ports.iter().filter(|(_, p)| p.node == node && p.out == out && !p.monitor).map(|(i, p)| (*i, p)).collect();
        v.sort_by(|a, b| a.1.channel.cmp(&b.1.channel).then(a.0.cmp(&b.0)));
        v
    }

    fn send(&mut self, from: u32, to: u32, on: bool) {
        if !on {
            let ids: Vec<u32> = self.links.iter().filter(|(_, l)| l.out_node == from && l.in_node == to).map(|(i, _)| *i).collect();
            for id in ids {
                let _ = self.registry.destroy_global(id);
            }
            return;
        }
        if self.links.values().any(|l| l.out_node == from && l.in_node == to) {
            return;
        }
        let outs = self.ports_of(from, true);
        let ins = self.ports_of(to, false);
        if outs.is_empty() || ins.is_empty() {
            eprintln!("send: no ports on {from} or {to}");
            return;
        }
        let mut pairs: Vec<(u32, u32)> = Vec::new();
        for (i, (op, o)) in outs.iter().enumerate() {
            let ip = ins
                .iter()
                .find(|(_, p)| !o.channel.is_empty() && p.channel == o.channel)
                .or_else(|| ins.get(i))
                .or_else(|| ins.first());
            if let Some((ip, _)) = ip {
                pairs.push((*op, *ip));
            }
        }
        for (op, ip) in pairs {
            let mut p = PropertiesBox::new();
            p.insert("link.output.node", from.to_string());
            p.insert("link.input.node", to.to_string());
            p.insert("link.output.port", op.to_string());
            p.insert("link.input.port", ip.to_string());
            p.insert("object.linger", "true");
            if let Err(e) = self.core.create_object::<Link>("link-factory", &p) {
                eprintln!("link {op}->{ip} failed: {e}");
            }
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::SetStreamVolume(id, v) => {
                if let Some(t) = self.nodes.get(&id) {
                    let v = v.clamp(0.0, 1.5);
                    let n = t.vol.borrow().channels.len().max(2);
                    set_props(&t.node, vec![Property::new(spa::sys::SPA_PROP_channelVolumes, Value::ValueArray(ValueArray::Float(vec![v; n])))]);
                    // Optimistic local update so the UI doesn't snap back before PipeWire echoes it.
                    t.vol.borrow_mut().channels = vec![v; n];
                }
                return;
            }
            Cmd::SetStreamMute(id, m) => {
                if let Some(t) = self.nodes.get(&id) {
                    set_props(&t.node, vec![Property::new(spa::sys::SPA_PROP_mute, Value::Bool(m))]);
                    t.vol.borrow_mut().mute = m;
                }
                return;
            }
            Cmd::RouteStream(id, sink) => {
                self.route(id, sink);
                return;
            }
            Cmd::SendStream(a, b, on) => {
                self.send(a, b, on);
                return;
            }
            Cmd::PromoteStream(id) => {
                let Some(t) = self.nodes.get(&id) else { return };
                let app = t.prop("application.name");
                let (name, rule) = if !app.is_empty() {
                    (app.clone(), MatchRule { app_name: Some(app), ..Default::default() })
                } else {
                    (t.label(), MatchRule { node_name: Some(t.prop("node.name")), ..Default::default() })
                };
                let from = t.source.clone();
                let mut cfg = self.engine.config().clone();
                // Don't promote something that already has its own tier.
                if cfg.sources.iter().any(|s| s.rule == rule) {
                    return;
                }
                let after = from.and_then(|n| cfg.sources.iter().position(|s| s.name == n));
                cfg.insert_source_after(after, &name, rule);
                eprintln!("promoted {name:?} to its own priority");
                self.apply_config(cfg);
                return;
            }
            Cmd::SetEnabled(on) => {
                self.engine.set_enabled(on);
                return;
            }
            _ => {}
        }
        let mut cfg = self.engine.config().clone();
        match cmd {
            Cmd::SetPriority(order) => cfg.set_priority(&order),
            Cmd::SetDepth(name, d) => {
                if let Some(s) = cfg.sources.iter_mut().find(|s| s.name == name) {
                    s.depth_db = d.map(|v| v.min(0.0));
                }
            }
            Cmd::SetBehavior(name, b) => {
                if let Some(s) = cfg.sources.iter_mut().find(|s| s.name == name) {
                    s.behavior = b;
                }
            }
            Cmd::SetDefaultDepth(d) => cfg.ducking.depth_db = d.min(0.0),
            Cmd::RemoveSource(n) => {
                cfg.remove_source(&n);
            }
            Cmd::Reload => match load_config() {
                Ok(c) => cfg = c,
                Err(e) => {
                    eprintln!("reload failed: {e:#}");
                    return;
                }
            },
            _ => return,
        }
        self.apply_config(cfg);
    }
}

fn watch_info(node: &Node, props: &Props, dirty: &Rc<Cell<bool>>) -> NodeListener {
    let (props, dirty) = (props.clone(), dirty.clone());
    node.add_listener_local()
        .info(move |info| {
            if let Some(p) = info.props() {
                let mut m = props.borrow_mut();
                for (k, v) in p.iter() {
                    m.insert(k.to_owned(), v.to_owned());
                }
                dirty.set(true);
            }
        })
        .register()
}

fn on_node(st: &Rc<RefCell<State>>, obj: &GlobalObject<&DictRef>) {
    let Some(props) = obj.props else { return };
    let Some(class) = props.get("media.class") else { return };
    let (registry, core, dirty) = {
        let s = st.borrow();
        (s.registry.clone(), s.core.clone(), s.dirty.clone())
    };

    if class == CLASS_SINK || class == CLASS_CAPTURE {
        let Ok(node) = registry.bind::<Node, _>(obj) else { return };
        let pm: Props = Rc::new(RefCell::new(dict_to_map(props)));
        let l = watch_info(&node, &pm, &dirty);
        st.borrow_mut().others.insert(obj.id, GNode { props: pm, _node: node, _listener: l });
        return;
    }
    if class != CLASS_PLAYBACK {
        return;
    }

    let Ok(node) = registry.bind::<Node, _>(obj) else { return };
    let serial = props.get("object.serial").unwrap_or_default().to_owned();
    let mut mprops = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "DSP",
        *pw::keys::NODE_NAME => "wavectl-meter",
        *pw::keys::NODE_PASSIVE => "true",
        *pw::keys::NODE_DONT_RECONNECT => "true",
    };
    mprops.insert("target.object", serial.as_str());
    mprops.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
    let Ok(meter) = StreamBox::new(&core, "wavectl-meter", mprops) else { return };

    let peaks = Rc::new(Cell::new((0f32, 0f32)));
    let chans = Rc::new(Cell::new(2u32));
    let (p2, c2, c3) = (peaks.clone(), chans.clone(), chans.clone());
    let Ok(ml) = meter
        .add_local_listener_with_user_data(())
        .param_changed(move |_, _, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let mut info = spa::param::audio::AudioInfoRaw::new();
            if info.parse(param).is_ok() {
                c3.set(info.channels().max(1));
            }
        })
        .process(move |stream, _| {
            let Some(mut buf) = stream.dequeue_buffer() else { return };
            let c = c2.get().max(1) as usize;
            let (mut l, mut r) = p2.get();
            for d in buf.datas_mut() {
                let n = d.chunk().size() as usize;
                let Some(bytes) = d.data() else { continue };
                let f = |b: &[u8]| f32::from_le_bytes([b[0], b[1], b[2], b[3]]).abs();
                for frame in bytes[..n.min(bytes.len())].chunks_exact(4 * c) {
                    let a = f(&frame[0..4]);
                    let b = if c > 1 { f(&frame[4..8]) } else { a };
                    l = l.max(a);
                    r = r.max(b);
                }
            }
            p2.set((l, r));
        })
        .register()
    else {
        return;
    };
    let mut audio = spa::param::audio::AudioInfoRaw::new();
    audio.set_format(spa::param::audio::AudioFormat::F32LE);
    let obj_pod = Object { type_: spa::sys::SPA_TYPE_OBJECT_Format, id: spa::sys::SPA_PARAM_EnumFormat, properties: audio.into() };
    let Ok((cursor, _)) = PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj_pod)) else { return };
    let bytes = cursor.into_inner();
    let mut params = [Pod::from_bytes(&bytes).unwrap()];
    if let Err(e) = meter.connect(
        Direction::Input,
        None,
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
        &mut params,
    ) {
        eprintln!("meter connect failed for node {}: {e}", obj.id);
        return;
    }

    let pm: Props = Rc::new(RefCell::new(dict_to_map(props)));
    let vol = Rc::new(RefCell::new(Vol::default()));
    let nl = {
        let (pm, dirty, vol) = (pm.clone(), dirty.clone(), vol.clone());
        node.add_listener_local()
            .info(move |info| {
                if let Some(p) = info.props() {
                    let mut m = pm.borrow_mut();
                    for (k, v) in p.iter() {
                        m.insert(k.to_owned(), v.to_owned());
                    }
                    dirty.set(true);
                }
            })
            .param(move |_seq, id, _idx, _next, param| {
                if id == spa::param::ParamType::Props {
                    if let Some(p) = param {
                        parse_props(p, &mut vol.borrow_mut());
                    }
                }
            })
            .register()
    };
    node.subscribe_params(&[spa::param::ParamType::Props]);
    // The stream borrows `core`, which lives for the whole program.
    let meter: StreamBox<'static> = unsafe { std::mem::transmute(meter) };
    eprintln!("+ stream {} {:?}", obj.id, props.get("node.name").unwrap_or_default());
    let mut s = st.borrow_mut();
    s.nodes.insert(
        obj.id,
        Tracked {
            props: pm,
            vol,
            disp: (-90.0, -90.0),
            det: -90.0,
            source: None,
            node,
            _node_listener: nl,
            _meter: meter,
            _meter_listener: ml,
            peaks,
            last_gain_db: 0.0,
        },
    );
    s.dirty.set(true);
}

fn on_global(st: &Rc<RefCell<State>>, obj: &GlobalObject<&DictRef>) {
    match obj.type_ {
        ObjectType::Node => on_node(st, obj),
        ObjectType::Port => {
            let Some(p) = obj.props else { return };
            let Some(node) = p.get("node.id").and_then(|v| v.parse().ok()) else { return };
            st.borrow_mut().ports.insert(
                obj.id,
                PortInfo {
                    node,
                    out: p.get("port.direction") == Some("out"),
                    channel: p.get("audio.channel").unwrap_or_default().to_owned(),
                    monitor: p.get("port.monitor") == Some("true"),
                },
            );
        }
        ObjectType::Link => {
            let Some(p) = obj.props else { return };
            let g = |k: &str| p.get(k).and_then(|v| v.parse::<u32>().ok());
            if let (Some(out_node), Some(in_node)) = (g("link.output.node"), g("link.input.node")) {
                st.borrow_mut().links.insert(obj.id, LinkInfo { out_node, in_node });
            }
        }
        ObjectType::Metadata => {
            let Some(p) = obj.props else { return };
            if p.get("metadata.name") != Some("default") {
                return;
            }
            let registry = st.borrow().registry.clone();
            let Ok(md) = registry.bind::<Metadata, _>(obj) else { return };
            let ds = st.borrow().default_sink.clone();
            let l = md
                .add_listener_local()
                .property(move |_subject, key, _type, value| {
                    if key == Some("default.audio.sink") {
                        let name = value
                            .and_then(|v| serde_json::from_str::<serde_json::Value>(v).ok())
                            .and_then(|j| j.get("name").and_then(|n| n.as_str().map(str::to_owned)));
                        *ds.borrow_mut() = name;
                    }
                    0
                })
                .register();
            st.borrow_mut().metadata = Some((md, l));
        }
        _ => {}
    }
}

fn main() -> Result<()> {
    pw::init();
    let cfg = load_config()?;

    let (tx, rx) = mpsc::channel::<Cmd>();
    let (mtx, mrx) = tokio::sync::mpsc::unbounded_channel::<MprisCmd>();
    let shared = Shared::new(tx, mtx);
    {
        let sh = shared.clone();
        std::thread::spawn(move || ipc::run(sh, mrx));
    }

    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;

    let state = Rc::new(RefCell::new(State {
        core: core.clone(),
        registry: registry.clone(),
        engine: Engine::new(cfg),
        nodes: HashMap::new(),
        others: HashMap::new(),
        ports: HashMap::new(),
        links: HashMap::new(),
        default_sink: Rc::new(RefCell::new(None)),
        metadata: None,
        dirty: Rc::new(Cell::new(true)),
        paused: Default::default(),
    }));

    let _reg_listener = {
        let (st, st2) = (state.clone(), state.clone());
        registry
            .add_listener_local()
            .global(move |obj| on_global(&st, obj))
            .global_remove(move |id| {
                let mut s = st2.borrow_mut();
                if let Some(t) = s.nodes.remove(&id) {
                    eprintln!("- stream {id} {:?}", t.prop("node.name"));
                }
                s.others.remove(&id);
                s.ports.remove(&id);
                s.links.remove(&id);
            })
            .register()
    };

    // Engine tick: ~30 Hz.
    let start = Instant::now();
    let timer = mainloop.loop_().add_timer({
        let st = state.clone();
        let shared = shared.clone();
        move |_| {
            let mut s = st.borrow_mut();
            while let Ok(c) = rx.try_recv() {
                s.handle(c);
            }
            if s.dirty.get() {
                s.reclassify();
            }
            let names: Vec<String> = s.engine.config().sources.iter().map(|x| x.name.clone()).collect();
            let n = names.len();
            let mut levels: Vec<Option<f32>> = vec![None; n];
            let mut raw_nodes: Vec<Vec<String>> = vec![vec![]; n];
            let mut streams: Vec<Vec<StreamView>> = vec![vec![]; n];
            let gains: Vec<f32> = s.engine.status().iter().map(|x| x.gain_db).collect();

            // Outputs and recording programs, for routing.
            let default_sink = s.default_sink.borrow().clone();
            let mut outputs: Vec<OutputView> = vec![];
            let mut captures: Vec<CaptureView> = vec![];
            for (id, g) in &s.others {
                let p = g.props.borrow();
                let get = |k: &str| p.get(k).cloned().unwrap_or_default();
                match get("media.class").as_str() {
                    CLASS_SINK => {
                        let desc = [get("node.description"), get("node.nick"), get("node.name")].into_iter().find(|x| !x.is_empty()).unwrap_or_default();
                        outputs.push(OutputView { id: *id, name: desc, is_default: default_sink.as_deref() == Some(get("node.name").as_str()) });
                    }
                    CLASS_CAPTURE => {
                        let nm = get("node.name");
                        let app = get("application.name");
                        let media = get("media.name");
                        // Skip ourselves and level-meter taps (GNOME Settings, etc.).
                        if nm == "wavectl-meter" || media == "Peak detect" || get("media.role") == "DSP" || app.is_empty() && nm.is_empty() {
                            continue;
                        }
                        captures.push(CaptureView { id: *id, name: if app.is_empty() { nm } else { app }, detail: media });
                    }
                    _ => {}
                }
            }
            outputs.sort_by(|a, b| a.name.cmp(&b.name));
            captures.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));

            let cap_ids: std::collections::HashSet<u32> = captures.iter().map(|c| c.id).collect();
            // Which sink/recorder each stream is linked into.
            let link_targets: HashMap<u32, Vec<u32>> = s.links.values().fold(HashMap::new(), |mut m, l| {
                m.entry(l.out_node).or_default().push(l.in_node);
                m
            });
            let class_of = |s: &State, id: u32| s.others.get(&id).and_then(|g| g.props.borrow().get("media.class").cloned());

            let players = shared.players.lock().unwrap().clone();
            let mut ids: Vec<u32> = s.nodes.keys().copied().collect();
            ids.sort_unstable();
            for id in ids {
                let (sink_id, sends) = {
                    let (mut sink, mut sends) = (None, vec![]);
                    for tgt in link_targets.get(&id).into_iter().flatten() {
                        match class_of(&s, *tgt).as_deref() {
                            Some(CLASS_SINK) => sink = sink.or(Some(*tgt)),
                            Some(CLASS_CAPTURE) if cap_ids.contains(tgt) => sends.push(*tgt),
                            _ => {}
                        }
                    }
                    sends.sort_unstable();
                    sends.dedup();
                    (sink, sends)
                };
                let t = s.nodes.get_mut(&id).unwrap();
                let (pl, pr) = t.peaks.replace((0.0, 0.0));
                let Some(i) = t.source.as_ref().and_then(|nm| names.iter().position(|x| x == nm)) else { continue };
                // Meter sees the post-duck signal; add the applied duck back.
                let raw = |p: f32| 20.0 * p.max(1e-9).log10();
                let (dl, dr) = (raw(pl), raw(pr));
                // Engine wants the pre-duck level: add the applied duck back. It sees the
                // peak-held detection level, not the raw per-tick peak.
                t.det = dl.max(dr).max(t.det - 3.0).max(-90.0);
                let db = t.det - gains[i];
                levels[i] = Some(levels[i].map_or(db, |l: f32| l.max(db)));
                let nn = t.prop("node.name");
                raw_nodes[i].push(nn);
                // Display level: instant attack, ~40 dB/s decay.
                t.disp = ((dl).max(t.disp.0 - 1.3).max(-90.0), (dr).max(t.disp.1 - 1.3).max(-90.0));
                let prop = |k: &str| t.prop(k);
                let bt = !prop("api.bluez5.address").is_empty();
                let name = if bt {
                    t.label()
                } else {
                    [prop("application.name"), prop("node.description"), t.label()].into_iter().find(|x| !x.is_empty()).unwrap_or_default()
                };
                let media = prop("media.name");
                let v = t.vol.borrow();
                streams[i].push(StreamView {
                    id,
                    detail: if media == name || media == "Playback" || media == "Audio Stream" { String::new() } else { media },
                    name,
                    volume: v.level(),
                    muted: v.mute,
                    level_db: Some(t.disp.0.max(t.disp.1)),
                    level_l: t.disp.0,
                    level_r: t.disp.1,
                    sink_id,
                    sends,
                    player: {
                        let h = t.hint();
                        players.iter().find(|p| player_matches(p, &h)).map(|p| PlayerView {
                            id: p.id.clone(),
                            name: p.name.clone(),
                            status: p.status.clone(),
                            title: p.title.clone(),
                            artist: p.artist.clone(),
                            album: p.album.clone(),
                        })
                    },
                });
            }
            let now = start.elapsed().as_millis() as u64;
            s.engine.tick(now, &levels);
            let status: Vec<_> = s.engine.status().to_vec();

            // "Pause" behavior: pause a suppressed source's players, resume when released.
            for st in &status {
                let want = st.suppressed && st.behavior == Behavior::Pause;
                let was = s.paused.contains(&st.name);
                if want && !was {
                    let hints: Vec<Hint> = s
                        .nodes
                        .values()
                        .filter(|t| t.source.as_deref() == Some(st.name.as_str()))
                        .map(|t| t.hint())
                        .collect();
                    let _ = shared.mpris.send(MprisCmd::Pause { source: st.name.clone(), hints });
                    s.paused.insert(st.name.clone());
                } else if !want && was {
                    let _ = shared.mpris.send(MprisCmd::Resume { source: st.name.clone() });
                    s.paused.remove(&st.name);
                }
            }
            let live: std::collections::HashSet<&String> = status.iter().map(|x| &x.name).collect();
            let gone: Vec<String> = s.paused.iter().filter(|n| !live.contains(n)).cloned().collect();
            for n in gone {
                let _ = shared.mpris.send(MprisCmd::Resume { source: n.clone() });
                s.paused.remove(&n);
            }
            let idx_of = |nm: &Option<String>| nm.as_ref().and_then(|x| names.iter().position(|y| y == x));
            for t in s.nodes.values_mut() {
                let g = idx_of(&t.source).map_or(0.0, |i| status[i].gain_db);
                if (g - t.last_gain_db).abs() > 0.05 || (g == 0.0 && t.last_gain_db != 0.0) {
                    set_soft_volume(&t.node, g);
                    t.last_gain_db = g;
                }
            }

            // Publish a snapshot for D-Bus clients.
            let cfg = s.engine.config();
            let view = StateView {
                enabled: s.engine.enabled(),
                default_depth_db: cfg.ducking.depth_db,
                threshold_db: cfg.ducking.threshold_db,
                ducking: status.iter().any(|x| x.gain_db < -0.5),
                outputs,
                captures,
                sources: status
                    .iter()
                    .enumerate()
                    .map(|(i, x)| SourceView {
                        name: x.name.clone(),
                        state: match x.state {
                            SourceState::Idle => "idle",
                            SourceState::Pending { .. } => "pending",
                            SourceState::Active => "active",
                            SourceState::Hold { .. } => "hold",
                        }
                        .into(),
                        active: x.state.is_active(),
                        gain_db: x.gain_db,
                        ducked: x.gain_db < -0.5,
                        level_db: levels[i],
                        depth_db: cfg.depth_for(i),
                        depth_override: cfg.sources[i].depth_db.is_some(),
                        kind: cfg.sources[i].rule.kind().into(),
                        behavior: x.behavior.as_str().into(),
                        behavior_override: cfg.sources[i].behavior.is_some(),
                        suppressed: x.suppressed,
                        paused: s.paused.contains(&x.name),
                        nodes: raw_nodes[i].clone(),
                        streams: std::mem::take(&mut streams[i]),
                        bluez_address: cfg.sources[i].rule.bluez_address.clone(),
                    })
                    .collect(),
            };
            *shared.state.lock().unwrap() = view;
        }
    });
    timer.update_timer(Some(Duration::from_millis(30)), Some(Duration::from_millis(30))).into_result()?;

    // Reset ducking on exit.
    {
        let st = state.clone();
        let ml = mainloop.clone();
        let reset = move || {
            for t in st.borrow().nodes.values() {
                if t.last_gain_db != 0.0 {
                    set_soft_volume(&t.node, 0.0);
                }
            }
            ml.quit();
        };
        let r2 = reset.clone();
        let _sigint = mainloop.loop_().add_signal_local(pw::loop_::Signal::INT, move || r2());
        let _sigterm = mainloop.loop_().add_signal_local(pw::loop_::Signal::TERM, reset);
        mainloop.run();
    }
    Ok(())
}
