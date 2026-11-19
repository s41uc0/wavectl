use crate::config::{Behavior, Config};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SourceState {
    Idle,
    /// Above threshold, waiting out `attack_ms`.
    Pending { since: u64 },
    Active,
    /// Dropped below threshold, still counted active until `hold_ms` passes.
    Hold { since: u64 },
}

impl SourceState {
    pub fn is_active(self) -> bool {
        matches!(self, SourceState::Active | SourceState::Hold { .. })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceStatus {
    pub name: String,
    pub state: SourceState,
    /// Duck gain currently applied (dB, <= 0).
    pub gain_db: f32,
    /// Where the gain is heading (0 or the source's depth).
    pub target_db: f32,
    /// A higher-priority source is active (and the engine is enabled). What
    /// that means for this source depends on its behavior.
    pub suppressed: bool,
    pub behavior: Behavior,
}

pub struct Engine {
    cfg: Config,
    status: Vec<SourceStatus>,
    last_tick: Option<u64>,
    enabled: bool,
}

impl Engine {
    pub fn new(cfg: Config) -> Self {
        let status = Self::fresh(&cfg);
        Self { cfg, status, last_tick: None, enabled: true }
    }

    fn fresh(cfg: &Config) -> Vec<SourceStatus> {
        cfg.sources
            .iter()
            .map(|s| SourceStatus { name: s.name.clone(), state: SourceState::Idle, gain_db: 0.0, target_db: 0.0, suppressed: false, behavior: s.behavior.unwrap_or_default() })
            .collect()
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Replace config, carrying runtime state over by source name so a
    /// priority reorder or reload doesn't glitch levels.
    pub fn set_config(&mut self, cfg: Config) {
        let old = std::mem::take(&mut self.status);
        self.status = Self::fresh(&cfg);
        for st in &mut self.status {
            if let Some(o) = old.iter().find(|o| o.name == st.name) {
                st.state = o.state;
                st.gain_db = o.gain_db;
            }
        }
        self.cfg = cfg;
    }

    /// Disabled engine ramps everything back to 0 dB.
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn status(&self) -> &[SourceStatus] {
        &self.status
    }

    /// Advance by one measurement. `levels[i]` is the current level in dBFS of
    /// source `i` in priority order (`None` = nothing connected/playing). The
    /// level must be the *pre-duck* level; callers metering post-gain should add
    /// back `status()[i].gain_db`.
    pub fn tick(&mut self, now_ms: u64, levels: &[Option<f32>]) -> &[SourceStatus] {
        let d = self.cfg.ducking.clone();
        let dt = self.last_tick.map_or(0, |t| now_ms.saturating_sub(t)) as f32;
        self.last_tick = Some(now_ms);

        for (i, st) in self.status.iter_mut().enumerate() {
            let above = levels.get(i).copied().flatten().is_some_and(|l| l > d.threshold_db);
            st.state = match (st.state, above) {
                (SourceState::Idle, true) => {
                    if d.attack_ms == 0 { SourceState::Active } else { SourceState::Pending { since: now_ms } }
                }
                (SourceState::Idle, false) => SourceState::Idle,
                (SourceState::Pending { .. }, false) => SourceState::Idle,
                (SourceState::Pending { since }, true) => {
                    if now_ms.saturating_sub(since) >= d.attack_ms as u64 { SourceState::Active } else { SourceState::Pending { since } }
                }
                (SourceState::Active, true) => SourceState::Active,
                (SourceState::Active, false) => SourceState::Hold { since: now_ms },
                (SourceState::Hold { .. }, true) => SourceState::Active,
                (SourceState::Hold { since }, false) => {
                    if now_ms.saturating_sub(since) >= d.hold_ms as u64 { SourceState::Idle } else { SourceState::Hold { since } }
                }
            };
        }

        let mut higher_active = false;
        for i in 0..self.status.len() {
            let behavior = self.cfg.behavior_for(i);
            let suppressed = self.enabled && higher_active;
            // Duck uses the configured depth, mute goes (nearly) silent; pause and
            // nothing leave the level alone (pausing is done by the caller).
            let depth = match behavior {
                Behavior::Duck => self.cfg.depth_for(i),
                Behavior::Mute => -60.0,
                Behavior::Pause | Behavior::Nothing => 0.0,
            };
            let target = if suppressed { depth } else { 0.0 };
            let st = &mut self.status[i];
            st.suppressed = suppressed;
            st.behavior = behavior;
            st.target_db = target;
            let down_rate = depth.abs() / d.duck_ms.max(1) as f32; // dB per ms
            let up_rate = depth.abs() / d.release_ms.max(1) as f32;
            if st.gain_db > target {
                st.gain_db = (st.gain_db - down_rate * dt).max(target);
            } else if st.gain_db < target {
                st.gain_db = (st.gain_db + up_rate * dt).min(target);
            }
            higher_active |= st.state.is_active();
        }
        &self.status
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matching::MatchRule;
    use crate::config::{Ducking, SourceConfig};

    fn cfg3() -> Config {
        let src = |n: &str| SourceConfig { name: n.into(), rule: MatchRule::default(), depth_db: None, behavior: None };
        Config { ducking: Ducking::default(), sources: vec![src("Laptop"), src("PS"), src("Phone")] }
    }

    const LOUD: Option<f32> = Some(-20.0);
    const QUIET: Option<f32> = Some(-80.0);

    /// Run `ms` of constant levels at 10 ms steps starting at `*t`.
    fn run(e: &mut Engine, t: &mut u64, ms: u64, lv: &[Option<f32>]) {
        for _ in 0..(ms / 10) {
            *t += 10;
            e.tick(*t, lv);
        }
    }

    fn gains(e: &Engine) -> Vec<f32> {
        e.status().iter().map(|s| s.gain_db).collect()
    }

    #[test]
    fn nothing_ducks_when_alone() {
        let mut e = Engine::new(cfg3());
        let mut t = 0;
        run(&mut e, &mut t, 2000, &[QUIET, QUIET, LOUD]);
        assert_eq!(gains(&e), vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn priority_matrix() {
        // (laptop, ps, phone active) -> expected targets
        for mask in 0..8u8 {
            let act = [mask & 1 != 0, mask & 2 != 0, mask & 4 != 0];
            let lv: Vec<_> = act.iter().map(|&a| if a { LOUD } else { QUIET }).collect();
            let mut e = Engine::new(cfg3());
            let mut t = 0;
            run(&mut e, &mut t, 3000, &lv);
            for i in 0..3 {
                let higher = act[..i].iter().any(|&a| a);
                let want = if higher { -15.0 } else { 0.0 };
                assert!((e.status()[i].gain_db - want).abs() < 1e-3, "mask {mask} src {i}: {:?}", gains(&e));
            }
        }
    }

    #[test]
    fn ps_beats_phone_but_not_laptop() {
        let mut e = Engine::new(cfg3());
        let mut t = 0;
        run(&mut e, &mut t, 1000, &[QUIET, LOUD, LOUD]);
        assert_eq!(gains(&e), vec![0.0, 0.0, -15.0]);
        run(&mut e, &mut t, 1000, &[LOUD, LOUD, LOUD]);
        assert_eq!(gains(&e), vec![0.0, -15.0, -15.0]);
    }

    #[test]
    fn attack_delay_ignores_blips() {
        let mut e = Engine::new(cfg3());
        let mut t = 0;
        run(&mut e, &mut t, 100, &[LOUD, QUIET, LOUD]); // < attack_ms 150
        run(&mut e, &mut t, 100, &[QUIET, QUIET, LOUD]);
        assert_eq!(gains(&e)[2], 0.0);
    }

    #[test]
    fn hold_bridges_gaps_then_releases() {
        let mut e = Engine::new(cfg3());
        let mut t = 0;
        run(&mut e, &mut t, 1000, &[LOUD, QUIET, LOUD]);
        assert_eq!(e.status()[2].gain_db, -15.0);
        // 1 s gap < hold 1.5 s: stays ducked
        run(&mut e, &mut t, 1000, &[QUIET, QUIET, LOUD]);
        assert_eq!(e.status()[2].gain_db, -15.0);
        // hold (1.5 s) expires 500 ms in, then 800 ms release: recovered
        run(&mut e, &mut t, 1500, &[QUIET, QUIET, LOUD]);
        assert_eq!(e.status()[2].gain_db, 0.0);
    }

    #[test]
    fn ramps_are_gradual() {
        let mut e = Engine::new(cfg3());
        let mut t = 0;
        run(&mut e, &mut t, 200, &[LOUD, QUIET, LOUD]); // active at 150 ms
        let g = e.status()[2].gain_db;
        assert!(g < 0.0 && g > -15.0, "mid-ramp, got {g}");
    }

    #[test]
    fn per_source_depth_and_disable() {
        let mut c = cfg3();
        c.sources[2].depth_db = Some(-30.0);
        let mut e = Engine::new(c);
        let mut t = 0;
        run(&mut e, &mut t, 1000, &[LOUD, QUIET, LOUD]);
        assert_eq!(e.status()[2].gain_db, -30.0);
        e.set_enabled(false);
        run(&mut e, &mut t, 1500, &[LOUD, QUIET, LOUD]);
        assert_eq!(e.status()[2].gain_db, 0.0);
    }

    #[test]
    fn specific_source_beats_group() {
        use crate::matching::NodeInfo;
        let mut c = Config::default_hub(); // "Laptop": all local apps
        let node = |app: &str| {
            let mut n = NodeInfo { node_name: app.into(), is_app_stream: true, ..Default::default() };
            n.props.insert("application.name".into(), app.into());
            n
        };
        assert_eq!(c.classify(&node("Spotify")), Some(0));
        // Promote Spotify below the group: it still wins its own streams.
        let i = c.insert_source_after(Some(0), "Spotify", MatchRule { app_name: Some("Spotify".into()), ..Default::default() });
        assert_eq!(i, 1);
        assert_eq!(c.classify(&node("Spotify")), Some(1));
        assert_eq!(c.classify(&node("Firefox")), Some(0));
        // Unique names, inserted after the requested index.
        let j = c.insert_source_after(Some(0), "Spotify", MatchRule { app_name: Some("x".into()), ..Default::default() });
        assert_eq!((j, c.sources[1].name.as_str()), (1, "Spotify 2"));
    }

    #[test]
    fn behaviors() {
        let mut c = cfg3();
        c.sources[1].behavior = Some(Behavior::Pause);
        c.sources[2].behavior = Some(Behavior::Mute);
        let mut e = Engine::new(c);
        let mut t = 0;
        run(&mut e, &mut t, 2000, &[LOUD, LOUD, LOUD]);
        let st = e.status();
        // Pause: level untouched, but flagged so the caller can pause the player.
        assert!(st[1].suppressed && st[1].gain_db == 0.0 && st[1].behavior == Behavior::Pause);
        // Mute: driven to -60 dB.
        assert!(st[2].suppressed && st[2].gain_db == -60.0);
        // Top source is never suppressed.
        assert!(!st[0].suppressed);
        // Release when the higher source stops.
        run(&mut e, &mut t, 4000, &[QUIET, QUIET, LOUD]);
        assert!(!e.status()[1].suppressed && !e.status()[2].suppressed);
        // Behavior "nothing" never touches anything.
        let mut c = cfg3();
        c.sources[2].behavior = Some(Behavior::Nothing);
        let mut e = Engine::new(c);
        run(&mut e, &mut t, 2000, &[LOUD, QUIET, LOUD]);
        assert_eq!(e.status()[2].gain_db, 0.0);
    }

    #[test]
    fn behavior_config_roundtrip() {
        let c = Config::from_toml("[[source]]\nname='A'\nbehavior='pause'\n[source.match]\nlocal_apps=true\n").unwrap();
        assert_eq!(c.sources[0].behavior, Some(Behavior::Pause));
        assert!(c.to_toml().unwrap().contains("behavior = \"pause\""));
        assert_eq!(Behavior::parse("None"), Some(Behavior::Nothing));
    }

    #[test]
    fn reorder_keeps_state() {
        let mut e = Engine::new(cfg3());
        let mut t = 0;
        run(&mut e, &mut t, 1000, &[LOUD, QUIET, LOUD]);
        let mut c = e.config().clone();
        c.set_priority(&["Phone".into(), "Laptop".into()]);
        e.set_config(c);
        assert_eq!(e.status()[0].name, "Phone");
        assert_eq!(e.status()[1].name, "Laptop");
        assert_eq!(e.status()[2].name, "PS");
        assert_eq!(e.status()[0].state, SourceState::Active);
    }
}
