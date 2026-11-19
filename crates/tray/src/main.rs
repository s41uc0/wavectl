//! wavectl-tray: StatusNotifierItem that reflects ducking state; click opens the GUI.

use std::{
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use wavectl_client::Client;
use wavectl_core::api::StateView;
use ksni::{blocking::TrayMethods, menu::*, Category, ToolTip};

struct Tray {
    state: Option<StateView>,
}

fn open_gui() {
    // The GUI is single-instance: a second launch just raises the window.
    let exe = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("wavectl-gui")));
    let cmd = exe.filter(|p| p.exists()).unwrap_or_else(|| "wavectl-gui".into());
    let _ = Command::new(cmd).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

impl Tray {
    fn summary(&self) -> String {
        match &self.state {
            None => "wavectld is not running".into(),
            Some(s) => {
                let order = s.sources.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(" > ");
                let ducked: Vec<_> = s.sources.iter().filter(|x| x.ducked).map(|x| x.name.as_str()).collect();
                let mut t = order;
                if !s.enabled {
                    t.push_str("\nPriority rules paused");
                } else if !ducked.is_empty() {
                    t.push_str(&format!("\nDucking: {}", ducked.join(", ")));
                }
                t
            }
        }
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "wavectl".into()
    }
    fn title(&self) -> String {
        "wavectl".into()
    }
    fn category(&self) -> Category {
        Category::Hardware
    }
    fn icon_name(&self) -> String {
        match &self.state {
            None => "wavectl-paused-symbolic",
            Some(s) if !s.enabled => "wavectl-paused-symbolic",
            Some(s) if s.ducking => "wavectl-ducking-symbolic",
            Some(_) => "wavectl-symbolic",
        }
        .into()
    }
    fn icon_theme_path(&self) -> String {
        // <prefix>/bin/wavectl-tray -> <prefix>/share/wavectl/icons
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent()?.parent().map(|d| d.join("share/wavectl/icons")))
            .filter(|d| d.exists())
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
    fn tool_tip(&self) -> ToolTip {
        ToolTip { title: "wavectl".into(), description: self.summary(), ..Default::default() }
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        open_gui();
    }
    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        open_gui();
    }
    fn menu(&self) -> Vec<MenuItem<Self>> {
        let enabled = self.state.as_ref().is_some_and(|s| s.enabled);
        vec![
            StandardItem { label: "Open wavectl".into(), activate: Box::new(|_| open_gui()), ..Default::default() }.into(),
            CheckmarkItem {
                label: "Priority rules".into(),
                checked: enabled,
                enabled: self.state.is_some(),
                activate: Box::new(move |t: &mut Self| {
                    if let Ok(c) = Client::connect() {
                        let _ = c.set_enabled(!enabled);
                    }
                    if let Some(s) = t.state.as_mut() {
                        s.enabled = !enabled;
                    }
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit tray".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|_| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

fn main() {
    // Wait for the StatusNotifierWatcher (the shell may start after us at login).
    let handle = loop {
        match (Tray { state: None }).spawn() {
            Ok(h) => break h,
            Err(e) => {
                eprintln!("tray: cannot register ({e}); retrying");
                thread::sleep(Duration::from_secs(3));
            }
        }
    };
    let mut client: Option<Client> = None;
    loop {
        if client.is_none() {
            client = Client::connect().ok();
        }
        let st = client.as_ref().and_then(|c| c.state().ok());
        if st.is_none() {
            client = None;
        }
        // Only touch the tray when something visible changed.
        handle.update(|t: &mut Tray| {
            let key = |s: &Option<StateView>| s.as_ref().map(|s| (s.enabled, s.ducking, s.sources.iter().map(|x| (x.name.clone(), x.ducked)).collect::<Vec<_>>()));
            if key(&t.state) != key(&st) {
                t.state = st;
            }
        });
        thread::sleep(Duration::from_millis(400));
    }
}
