//! wavectl-gui: GTK4 window for priorities, per-stream volume, routing and behaviors.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

use wavectl_client::Client;
use wavectl_core::api::{SourceView, StateView, StreamView};
use gtk4 as gtk;
use gtk::{cairo, glib, prelude::*};

const CSS: &str = "
.tier { padding: 6px 0 8px 0; }
.tier-head { padding: 8px 12px 4px 12px; }
.stream-row { padding: 3px 12px 3px 46px; }
.ducked-badge { font-weight: bold; }
.rank { font-size: 1.4em; font-weight: 800; opacity: .45; min-width: 1.6em; }
.banner { padding: 8px 12px; }
.meter scale trough { background: transparent; border: none; min-height: 30px; }
.meter scale highlight { background: alpha(@accent_bg_color, .18); border: none; border-radius: 8px; }
.meter scale slider { min-width: 7px; min-height: 30px; border-radius: 4px; margin: 0; padding: 0; background: @accent_bg_color; border: none; box-shadow: none; outline: none; }
.stream-pct { font-variant-numeric: tabular-nums; min-width: 3.2em; }
";

/// Two-channel meter with falling peak markers.
struct Meter {
    l: f32,
    r: f32,
    peak_l: f32,
    peak_r: f32,
    hold_l: Instant,
    hold_r: Instant,
    last: Instant,
}

impl Meter {
    fn new() -> Self {
        let now = Instant::now();
        Meter { l: -90.0, r: -90.0, peak_l: -90.0, peak_r: -90.0, hold_l: now, hold_r: now, last: now }
    }

    /// Bars follow the daemon's smoothed levels; peaks hold briefly then fall.
    fn update(&mut self, l: f32, r: f32) {
        const HOLD: Duration = Duration::from_millis(700);
        const FALL_DB_PER_S: f32 = 22.0;
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f32();
        self.last = now;
        self.l = l;
        self.r = r;
        for (lev, peak, hold) in [(l, &mut self.peak_l, &mut self.hold_l), (r, &mut self.peak_r, &mut self.hold_r)] {
            if lev >= *peak {
                *peak = lev;
                *hold = now;
            } else if now.duration_since(*hold) > HOLD {
                *peak = (*peak - FALL_DB_PER_S * dt).max(lev).max(-90.0);
            }
        }
    }
}

fn db_pos(db: f32) -> f64 {
    ((db + 60.0) / 60.0).clamp(0.0, 1.0) as f64
}

fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, std::f64::consts::FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
    cr.arc(x + r, y + r, r, std::f64::consts::PI, 3.0 * std::f64::consts::FRAC_PI_2);
    cr.close_path();
}

fn draw_meter(area: &gtk::DrawingArea, cr: &cairo::Context, w: i32, h: i32, m: &Meter) {
    let (w, h) = (w as f64, h as f64);
    let fg = area.color();
    let (bar_h, gap) = ((h - 3.0 * 3.0) / 2.0, 3.0);
    for (i, (lev, peak)) in [(m.l, m.peak_l), (m.r, m.peak_r)].into_iter().enumerate() {
        let y = 3.0 + i as f64 * (bar_h + gap);
        rounded(cr, 0.0, y, w, bar_h, 3.0);
        cr.set_source_rgba(fg.red() as f64, fg.green() as f64, fg.blue() as f64, 0.10);
        let _ = cr.fill();
        let fill_w = w * db_pos(lev);
        if fill_w > 0.5 {
            let g = cairo::LinearGradient::new(0.0, 0.0, w, 0.0);
            g.add_color_stop_rgb(0.0, 0.25, 0.72, 0.42);
            g.add_color_stop_rgb(db_pos(-18.0), 0.30, 0.78, 0.40);
            g.add_color_stop_rgb(db_pos(-8.0), 0.95, 0.76, 0.22);
            g.add_color_stop_rgb(db_pos(-2.0), 0.92, 0.32, 0.28);
            g.add_color_stop_rgb(1.0, 0.92, 0.25, 0.25);
            rounded(cr, 0.0, y, fill_w, bar_h, 3.0);
            let _ = cr.set_source(&g);
            let _ = cr.fill();
        }
        if peak > -59.0 {
            let x = (w * db_pos(peak)).clamp(1.0, w - 1.0);
            cr.set_source_rgba(fg.red() as f64, fg.green() as f64, fg.blue() as f64, 0.85);
            cr.rectangle(x - 1.0, y, 2.0, bar_h);
            let _ = cr.fill();
        }
    }
}

struct StreamRow {
    detail: gtk::Label,
    play: Option<gtk::Button>,
    meter: Rc<RefCell<Meter>>,
    area: gtk::DrawingArea,
    scale: gtk::Scale,
    pct: gtk::Label,
    mute: gtk::ToggleButton,
    /// Last time the user moved the slider; daemon echoes are ignored briefly.
    touched: Rc<Cell<Option<Instant>>>,
}

struct SourceRow {
    status: gtk::Label,
    badge: gtk::Label,
    streams: Vec<StreamRow>,
}

struct Ui {
    window: gtk::ApplicationWindow,
    client: RefCell<Option<Client>>,
    updating: Cell<bool>,
    // header / footer
    enable: gtk::Switch,
    default_depth: gtk::SpinButton,
    banner: gtk::Revealer,
    toast: gtk::Label,
    // sources
    src_list: gtk::Box,
    src_sig: RefCell<String>,
    src_rows: RefCell<Vec<SourceRow>>,
    // bluetooth
    last_state: RefCell<StateView>,
}

impl Ui {
    fn say(&self, msg: &str) {
        self.toast.set_text(msg);
        let t = self.toast.clone();
        let m = msg.to_string();
        glib::timeout_add_local_once(std::time::Duration::from_secs(6), move || {
            if t.text() == m {
                t.set_text("");
            }
        });
    }

    /// Run `f` with a connected client; surfaces errors in the toast line.
    fn call<T>(&self, f: impl FnOnce(&Client) -> Result<T, String>) -> Option<T> {
        let mut c = self.client.borrow_mut();
        if c.is_none() {
            *c = Client::connect().ok();
        }
        match c.as_ref().map(f) {
            Some(Ok(v)) => Some(v),
            Some(Err(e)) => {
                if e.contains("not running") {
                    *c = None;
                }
                drop(c);
                self.say(&e);
                None
            }
            None => None,
        }
    }
}

fn spin(min: f64, max: f64, step: f64, v: f64) -> gtk::SpinButton {
    let s = gtk::SpinButton::with_range(min, max, step);
    s.set_value(v);
    s.set_valign(gtk::Align::Center);
    s
}

fn icon_btn(icon: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tip));
    b.set_valign(gtk::Align::Center);
    b.add_css_class("flat");
    b
}

fn clear(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

fn state_label(s: &SourceView) -> String {
    if s.streams.is_empty() {
        return "Not connected".into();
    }
    if s.paused {
        return "Paused while a higher source plays".into();
    }
    match (s.suppressed, s.behavior.as_str()) {
        (true, "duck") => format!("Ducked {:.0} dB by a higher source", s.gain_db),
        (true, "mute") => "Muted by a higher source".into(),
        _ if s.active => "Playing".into(),
        _ => "Connected, quiet".into(),
    }
}

/// Volume slider position is cube-root so it feels like GNOME/pavucontrol.
fn vol_to_pos(v: f32) -> f64 {
    (v.max(0.0) as f64).cbrt().min(1.0)
}

/// Per-stream routing: pick the output device and send this program into
/// other programs that are recording.
fn routing_button(ui: &Rc<Ui>, id: u32) -> gtk::MenuButton {
    let btn = gtk::MenuButton::new();
    btn.set_icon_name("emblem-shared-symbolic");
    btn.set_tooltip_text(Some("Route: choose output, or send into another program"));
    btn.set_valign(gtk::Align::Center);
    btn.add_css_class("flat");
    let pop = gtk::Popover::new();
    btn.set_popover(Some(&pop));
    pop.connect_show({
        let ui = ui.clone();
        move |pop| {
            let st = ui.last_state.borrow().clone();
            let Some(stream) = st.sources.iter().flat_map(|s| &s.streams).find(|x| x.id == id).cloned() else { return };
            let bx = gtk::Box::new(gtk::Orientation::Vertical, 8);
            bx.set_margin_start(12);
            bx.set_margin_end(12);
            bx.set_margin_top(12);
            bx.set_margin_bottom(12);
            bx.set_size_request(300, -1);

            let h1 = gtk::Label::new(Some("Output"));
            h1.set_xalign(0.0);
            h1.add_css_class("heading");
            bx.append(&h1);
            let mut names = vec!["Follow default output".to_string()];
            names.extend(st.outputs.iter().map(|o| if o.is_default { format!("{} (default)", o.name) } else { o.name.clone() }));
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let dd = gtk::DropDown::from_strings(&refs);
            let cur = stream.sink_id.and_then(|sid| st.outputs.iter().position(|o| o.id == sid)).map_or(0, |i| i as u32 + 1);
            dd.set_selected(cur);
            dd.connect_selected_notify({
                let (ui, outs) = (ui.clone(), st.outputs.clone());
                move |d| {
                    let sel = d.selected();
                    if sel == cur {
                        return;
                    }
                    let sink = if sel == 0 { 0 } else { outs.get(sel as usize - 1).map_or(0, |o| o.id) };
                    ui.call(|c| c.route_stream(id, sink));
                }
            });
            bx.append(&dd);

            let h2 = gtk::Label::new(Some("Also send into"));
            h2.set_xalign(0.0);
            h2.add_css_class("heading");
            h2.set_margin_top(6);
            bx.append(&h2);
            if st.captures.is_empty() {
                let l = gtk::Label::new(Some("No program is recording audio right now."));
                l.set_xalign(0.0);
                l.add_css_class("dim-label");
                l.add_css_class("caption");
                bx.append(&l);
            }
            for cap in &st.captures {
                let text = if cap.detail.is_empty() || cap.detail == cap.name { cap.name.clone() } else { format!("{} — {}", cap.name, cap.detail) };
                let cb = gtk::CheckButton::with_label(&text);
                cb.set_active(stream.sends.contains(&cap.id));
                let (ui, cid) = (ui.clone(), cap.id);
                cb.connect_toggled(move |b| {
                    let on = b.is_active();
                    ui.call(|c| c.send_stream(id, cid, on));
                });
                bx.append(&cb);
            }
            // Priority: promote the program out of its group into its own tier.
            let tier = st.sources.iter().position(|s| s.streams.iter().any(|x| x.id == id));
            if let Some(ti) = tier {
                let src = &st.sources[ti];
                let h3 = gtk::Label::new(Some("Priority"));
                h3.set_xalign(0.0);
                h3.add_css_class("heading");
                h3.set_margin_top(6);
                bx.append(&h3);
                if src.kind == "group" {
                    let b = gtk::Button::with_label(&format!("Give {} its own priority", stream.name));
                    b.set_tooltip_text(Some("Adds it as its own tier so it can duck, pause or mute independently of the group"));
                    let (ui, pop) = (ui.clone(), pop.clone());
                    b.connect_clicked(move |_| {
                        ui.call(|c| c.promote_stream(id));
                        pop.popdown();
                    });
                    bx.append(&b);
                } else {
                    let l = gtk::Label::new(Some(&format!("{} has its own tier (#{})", src.name, ti + 1)));
                    l.set_xalign(0.0);
                    l.set_wrap(true);
                    l.add_css_class("dim-label");
                    l.add_css_class("caption");
                    bx.append(&l);
                }
            }
            pop.set_child(Some(&bx));
        }
    });
    btn
}

/// "Title — Artist" when a media player is known, else the stream's own title.
fn detail_text(st: &StreamView) -> (String, String) {
    match &st.player {
        Some(p) if !p.title.is_empty() => {
            let text = if p.artist.is_empty() { p.title.clone() } else { format!("{} — {}", p.title, p.artist) };
            let tip = format!("{}: {}{}", p.name, text, if p.album.is_empty() { String::new() } else { format!(" ({})", p.album) });
            (text, tip)
        }
        _ => (st.detail.clone(), st.detail.clone()),
    }
}

fn stream_row(ui: &Rc<Ui>, st: &StreamView) -> (gtk::Box, StreamRow) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.add_css_class("stream-row");

    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text.set_size_request(170, -1);
    let name = gtk::Label::new(Some(&st.name));
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.set_max_width_chars(22);
    text.append(&name);
    let (dtext, dtip) = detail_text(st);
    let detail = gtk::Label::new(Some(&dtext));
    detail.set_xalign(0.0);
    detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
    detail.set_max_width_chars(26);
    detail.add_css_class("dim-label");
    detail.add_css_class("caption");
    detail.set_tooltip_text(Some(&dtip));
    detail.set_visible(!dtext.is_empty());
    text.append(&detail);
    text.set_valign(gtk::Align::Center);
    row.append(&text);

    // Slider drawn on top of the live meter.
    let meter = Rc::new(RefCell::new(Meter::new()));
    meter.borrow_mut().update(st.level_l, st.level_r);
    let area = gtk::DrawingArea::new();
    area.set_hexpand(true);
    area.set_content_height(34);
    area.set_valign(gtk::Align::Center);
    area.set_draw_func({
        let m = meter.clone();
        move |a, cr, w, h| draw_meter(a, cr, w, h, &m.borrow())
    });
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
    scale.set_draw_value(false);
    scale.set_hexpand(true);
    scale.set_valign(gtk::Align::Center);
    scale.set_value(vol_to_pos(st.volume));
    let overlay = gtk::Overlay::new();
    overlay.add_css_class("meter");
    overlay.set_hexpand(true);
    overlay.set_child(Some(&area));
    overlay.add_overlay(&scale);
    row.append(&overlay);

    let pct = gtk::Label::new(Some(&format!("{:.0}%", st.volume * 100.0)));
    pct.add_css_class("stream-pct");
    pct.set_xalign(1.0);
    row.append(&pct);

    let id = st.id;
    let mut play = None;
    if let Some(pl) = &st.player {
        let tb = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        tb.set_valign(gtk::Align::Center);
        let mk = |icon: &str, tip: &str, action: &'static str| {
            let b = icon_btn(icon, tip);
            let (ui, pid) = (ui.clone(), pl.id.clone());
            b.connect_clicked(move |_| {
                ui.call(|c| c.player_control(&pid, action));
            });
            b
        };
        tb.append(&mk("media-skip-backward-symbolic", "Previous track", "previous"));
        let pp = mk(
            if pl.status == "playing" { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" },
            "Play / pause",
            "playpause",
        );
        tb.append(&pp);
        tb.append(&mk("media-skip-forward-symbolic", "Next track", "next"));
        row.append(&tb);
        play = Some(pp);
    }

    let mute = gtk::ToggleButton::new();
    mute.set_icon_name("audio-volume-muted-symbolic");
    mute.set_tooltip_text(Some("Mute"));
    mute.set_valign(gtk::Align::Center);
    mute.add_css_class("flat");
    mute.set_active(st.muted);
    row.append(&mute);
    row.append(&routing_button(ui, st.id));

    let touched = Rc::new(Cell::new(None));
    scale.connect_value_changed({
        let (ui, pct, touched) = (ui.clone(), pct.clone(), touched.clone());
        move |sc| {
            if ui.updating.get() {
                return;
            }
            touched.set(Some(Instant::now()));
            let v = sc.value().powi(3);
            pct.set_text(&format!("{:.0}%", v * 100.0));
            ui.call(|c| c.set_stream_volume(id, v));
        }
    });
    mute.connect_toggled({
        let ui = ui.clone();
        move |b| {
            if !ui.updating.get() {
                let m = b.is_active();
                ui.call(|c| c.set_stream_mute(id, m));
            }
        }
    });
    (row, StreamRow { detail, play, meter, area, scale, pct, mute, touched })
}

fn rebuild_sources(ui: &Rc<Ui>, st: &StateView) {
    clear(&ui.src_list);
    let mut rows = Vec::new();
    let n = st.sources.len();
    for (i, s) in st.sources.iter().enumerate() {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        card.add_css_class("tier");

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.add_css_class("tier-head");

        let rank = gtk::Label::new(Some(&(i + 1).to_string()));
        rank.add_css_class("rank");
        row.append(&rank);

        let mid = gtk::Box::new(gtk::Orientation::Vertical, 1);
        mid.set_hexpand(true);
        mid.set_valign(gtk::Align::Center);
        let title = gtk::Label::new(Some(&s.name));
        title.set_xalign(0.0);
        title.add_css_class("heading");
        let status = gtk::Label::new(Some(&state_label(s)));
        status.set_xalign(0.0);
        status.add_css_class("dim-label");
        status.add_css_class("caption");
        mid.append(&title);
        mid.append(&status);
        row.append(&mid);

        let badge = gtk::Label::new(None);
        badge.add_css_class("ducked-badge");
        badge.add_css_class("warning");
        badge.set_width_chars(7);
        row.append(&badge);

        const BEHAVIORS: [(&str, &str); 4] = [("duck", "Duck"), ("pause", "Pause"), ("mute", "Mute"), ("nothing", "Nothing")];
        let labels: Vec<&str> = BEHAVIORS.iter().map(|b| b.1).collect();
        let beh = gtk::DropDown::from_strings(&labels);
        beh.set_valign(gtk::Align::Center);
        beh.set_tooltip_text(Some("What happens to this source while a higher one plays"));
        let cur = BEHAVIORS.iter().position(|b| b.0 == s.behavior).unwrap_or(0) as u32;
        beh.set_selected(cur);
        beh.connect_selected_notify({
            let (ui, name) = (ui.clone(), s.name.clone());
            move |d| {
                let sel = d.selected();
                if sel != cur && !ui.updating.get() {
                    let b = BEHAVIORS[sel as usize].0;
                    ui.call(|c| c.set_behavior(&name, b));
                }
            }
        });
        row.append(&beh);

        // Duck depth only matters for the duck behavior.
        let depth = spin(-60.0, 0.0, 1.0, s.depth_db as f64);
        depth.set_tooltip_text(Some("How far this source is lowered (dB) when a higher one plays"));
        depth.set_visible(s.behavior == "duck");
        if !s.depth_override {
            depth.add_css_class("dim-label");
        }
        let name = s.name.clone();
        depth.connect_value_changed({
            let ui = ui.clone();
            move |sp| {
                if !ui.updating.get() {
                    let v = sp.value();
                    ui.call(|c| c.set_depth(&name, v));
                }
            }
        });
        row.append(&depth);

        let reset = icon_btn("edit-undo-symbolic", "Use default depth");
        reset.set_sensitive(s.depth_override);
        reset.set_visible(s.behavior == "duck");
        reset.connect_clicked({
            let (ui, name) = (ui.clone(), s.name.clone());
            move |_| {
                ui.call(|c| c.clear_depth(&name));
            }
        });
        row.append(&reset);

        let up = icon_btn("go-up-symbolic", "Higher priority");
        up.set_sensitive(i > 0);
        let down = icon_btn("go-down-symbolic", "Lower priority");
        down.set_sensitive(i + 1 < n);
        for (btn, delta) in [(&up, -1i32), (&down, 1)] {
            btn.connect_clicked({
                let ui = ui.clone();
                let order: Vec<String> = st.sources.iter().map(|x| x.name.clone()).collect();
                move |_| {
                    let mut o = order.clone();
                    let j = (i as i32 + delta) as usize;
                    o.swap(i, j);
                    ui.call(|c| c.set_priority(o));
                }
            });
        }
        row.append(&up);
        row.append(&down);

        let rm = icon_btn("user-trash-symbolic", "Remove this tier (its programs fall back to their group, or it returns if the device reconnects)");
        rm.connect_clicked({
            let (ui, name) = (ui.clone(), s.name.clone());
            move |_| {
                ui.call(|c| c.remove_source(&name));
            }
        });
        row.append(&rm);
        card.append(&row);

        let mut stream_rows = Vec::new();
        for stv in &s.streams {
            let (w, sr) = stream_row(ui, stv);
            card.append(&w);
            stream_rows.push(sr);
        }

        ui.src_list.append(&card);
        if i + 1 < n {
            ui.src_list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        }
        rows.push(SourceRow { status, badge, streams: stream_rows });
    }
    if n == 0 {
        let l = gtk::Label::new(Some("No sources yet. Connect a device or play something."));
        l.add_css_class("dim-label");
        l.set_margin_top(40);
        ui.src_list.append(&l);
    }
    *ui.src_rows.borrow_mut() = rows;
}

fn update_sources(ui: &Rc<Ui>, st: &StateView) {
    ui.updating.set(true);
    ui.enable.set_active(st.enabled);
    if (ui.default_depth.value() - st.default_depth_db as f64).abs() > 0.01 && !ui.default_depth.has_focus() {
        ui.default_depth.set_value(st.default_depth_db as f64);
    }
    let sig: String = st
        .sources
        .iter()
        .map(|s| {
            let streams: String = s.streams.iter().map(|x| format!("{}:{}:{}:{},", x.id, x.name, x.detail, x.player.as_ref().map_or("", |p| p.id.as_str()))).collect();
            format!("{}|{}|{}|{}|[{}];", s.name, s.depth_db, s.depth_override, s.behavior, streams)
        })
        .collect();
    if sig != *ui.src_sig.borrow() {
        *ui.src_sig.borrow_mut() = sig;
        rebuild_sources(ui, st);
    }
    for (row, s) in ui.src_rows.borrow().iter().zip(&st.sources) {
        row.status.set_text(&state_label(s));
        for (sr, stv) in row.streams.iter().zip(&s.streams) {
            sr.meter.borrow_mut().update(stv.level_l, stv.level_r);
            sr.area.queue_draw();
            let (dtext, dtip) = detail_text(stv);
            if sr.detail.text() != dtext {
                sr.detail.set_text(&dtext);
                sr.detail.set_tooltip_text(Some(&dtip));
                sr.detail.set_visible(!dtext.is_empty());
            }
            if let (Some(btn), Some(pl)) = (&sr.play, &stv.player) {
                btn.set_icon_name(if pl.status == "playing" { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" });
            }
            let idle = sr.touched.get().is_none_or(|t| t.elapsed() > Duration::from_millis(800));
            if idle {
                if (sr.scale.value() - vol_to_pos(stv.volume)).abs() > 0.004 {
                    sr.scale.set_value(vol_to_pos(stv.volume));
                }
                sr.pct.set_text(&format!("{:.0}%", stv.volume * 100.0));
                if sr.mute.is_active() != stv.muted {
                    sr.mute.set_active(stv.muted);
                }
            }
        }
        row.badge.set_text(&if s.ducked { format!("{:.0} dB", s.gain_db) } else { String::new() });
    }
    ui.updating.set(false);
}

fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let sw = gtk::ScrolledWindow::new();
    sw.set_vexpand(true);
    sw.set_hscrollbar_policy(gtk::PolicyType::Never);
    sw.set_child(Some(child));
    sw
}

/// Pin the window to the top-right under the panel. GTK4 can't position its own
/// windows on Wayland, so this asks the `window-calls` GNOME extension to move
/// it. Silently does nothing if that extension isn't installed.
fn place_top_right(window: &gtk::ApplicationWindow) {
    use gtk::gio::prelude::ListModelExt;
    let Some(display) = gtk::gdk::Display::default() else { return };
    let Some(mon) = display.monitors().item(0).and_downcast::<gtk::gdk::Monitor>() else { return };
    let geo = mon.geometry();
    let w = window.clone();
    glib::timeout_add_local_once(Duration::from_millis(250), move || {
        let _ = &w;
        let pid = std::process::id();
        let geo = (geo.x(), geo.y(), geo.width());
        gtk::gio::spawn_blocking(move || -> Result<(), zbus::Error> {
            let conn = zbus::blocking::Connection::session()?;
            let p = zbus::blocking::Proxy::new(
                &conn,
                "org.gnome.Shell",
                "/org/gnome/Shell/Extensions/Windows",
                "org.gnome.Shell.Extensions.Windows",
            )?;
            let list: String = p.call("List", &())?;
            let wins: Vec<serde_json::Value> = serde_json::from_str(&list).unwrap_or_default();
            let Some(id) = wins
                .iter()
                .find(|w| w["pid"].as_u64() == Some(pid as u64) && w["wm_class"].as_str().is_some_and(|c| c.contains("wavectl")))
                .and_then(|w| w["id"].as_u64())
            else {
                return Ok(());
            };
            let id = id as u32;
            let rect: String = p.call("GetFrameRect", &(id,))?;
            let r: serde_json::Value = serde_json::from_str(&rect).unwrap_or_default();
            let width = r["width"].as_i64().unwrap_or(560) as i32;
            let (x, y) = (geo.0 + geo.2 - width - 12, geo.1 + 40);
            p.call::<_, _, ()>("Move", &(id, x, y))?;
            Ok(())
        });
    });
}

fn build(app: &gtk::Application) {
    if let Some(w) = app.active_window() {
        w.present();
        return;
    }
    if let Some(display) = gtk::gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        if let Some(d) = std::env::current_exe().ok().and_then(|p| p.parent()?.parent().map(|d| d.join("share/icons"))) {
            theme.add_search_path(d);
        }
    }
    gtk::Window::set_default_icon_name("wavectl");
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let window = gtk::ApplicationWindow::builder().application(app).title("wavectl").default_width(560).default_height(560).build();

    // ---- header
    let header = gtk::HeaderBar::new();
    let stack = gtk::Stack::new();
    let title = gtk::Label::new(Some("wavectl"));
    title.add_css_class("title");
    header.set_title_widget(Some(&title));
    let enable = gtk::Switch::new();
    enable.set_valign(gtk::Align::Center);
    enable.set_tooltip_text(Some("Turn priority behaviors (duck / pause / mute) on or off"));
    let en_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    en_box.append(&gtk::Label::new(Some("Priorities")));
    en_box.append(&enable);
    header.pack_end(&en_box);
    window.set_titlebar(Some(&header));

    // ---- sources page
    let src_list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let default_depth = spin(-60.0, 0.0, 1.0, -15.0);
    let foot = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    foot.set_margin_start(12);
    foot.set_margin_end(12);
    foot.set_margin_top(8);
    foot.set_margin_bottom(8);
    let hint = gtk::Label::new(Some("Top of the list wins. Lower sources duck by their depth while a higher one plays."));
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");
    hint.set_wrap(true);
    hint.set_xalign(0.0);
    hint.set_hexpand(true);
    foot.append(&hint);
    foot.append(&gtk::Label::new(Some("Default depth")));
    foot.append(&default_depth);
    let src_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    src_page.append(&scrolled(&src_list));
    src_page.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    src_page.append(&foot);
    stack.add_titled(&src_page, Some("sources"), "Sources");

    // ---- chrome: offline banner + toast
    let banner_label = gtk::Label::new(Some("wavectld is not running."));
    banner_label.set_hexpand(true);
    banner_label.set_xalign(0.0);
    let start = gtk::Button::with_label("Start");
    let banner_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    banner_box.add_css_class("banner");
    banner_box.add_css_class("error");
    banner_box.append(&banner_label);
    banner_box.append(&start);
    let banner = gtk::Revealer::new();
    banner.set_child(Some(&banner_box));
    start.connect_clicked(|_| {
        let _ = std::process::Command::new("systemctl").args(["--user", "start", "wavectld"]).spawn();
    });
    let toast = gtk::Label::new(None);
    toast.set_xalign(0.0);
    toast.set_margin_start(12);
    toast.set_margin_bottom(4);
    toast.add_css_class("caption");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&banner);
    root.append(&stack);
    root.append(&toast);
    window.set_child(Some(&root));

    let ui = Rc::new(Ui {
        window: window.clone(),
        client: RefCell::new(None),
        updating: Cell::new(false),
        enable: enable.clone(),
        default_depth: default_depth.clone(),
        banner: banner.clone(),
        toast,
        src_list,
        src_sig: RefCell::new(String::new()),
        src_rows: RefCell::new(vec![]),
        last_state: RefCell::new(StateView::default()),
    });

    enable.connect_state_set({
        let ui = ui.clone();
        move |_, on| {
            if !ui.updating.get() {
                ui.call(|c| c.set_enabled(on));
            }
            glib::Propagation::Proceed
        }
    });
    default_depth.connect_value_changed({
        let ui = ui.clone();
        move |s| {
            if !ui.updating.get() {
                let v = s.value();
                ui.call(|c| c.set_default_depth(v));
            }
        }
    });
    // ---- polling
    glib::timeout_add_local(std::time::Duration::from_millis(33), {
        let ui = ui.clone();
        move || {
            let st = {
                let mut c = ui.client.borrow_mut();
                if c.is_none() {
                    *c = Client::connect().ok();
                }
                c.as_ref().map(|c| c.state())
            };
            match st {
                Some(Ok(st)) => {
                    ui.banner.set_reveal_child(false);
                    update_sources(&ui, &st);
                    *ui.last_state.borrow_mut() = st;
                }
                _ => {
                    *ui.client.borrow_mut() = None;
                    ui.banner.set_reveal_child(true);
                }
            }
            glib::ControlFlow::Continue
        }
    });

    let esc = gtk::EventControllerKey::new();
    esc.connect_key_pressed({
        let w = ui.window.clone();
        move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                w.close();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        }
    });
    window.add_controller(esc);
    window.present();
    place_top_right(&window);
}

fn main() -> glib::ExitCode {
    let app = gtk::Application::builder().application_id("org.wavectl.Gui").build();
    app.connect_activate(build);
    app.run()
}
