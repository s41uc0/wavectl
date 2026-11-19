use serde::{Deserialize, Serialize};

use crate::matching::MatchRule;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("duplicate source name: {0}")]
    DuplicateSource(String),
    #[error("{0} must be a negative-or-zero dB value, got {1}")]
    BadDepth(String, f32),
}

/// What happens to a source while a higher-priority source is playing.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Behavior {
    /// Lower its volume by the duck depth.
    #[default]
    Duck,
    /// Pause its media player, resume afterwards.
    Pause,
    /// Silence it (volume to -60 dB), restore afterwards.
    Mute,
    /// Leave it alone.
    Nothing,
}

impl Behavior {
    pub fn as_str(self) -> &'static str {
        match self {
            Behavior::Duck => "duck",
            Behavior::Pause => "pause",
            Behavior::Mute => "mute",
            Behavior::Nothing => "nothing",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "duck" => Some(Behavior::Duck),
            "pause" => Some(Behavior::Pause),
            "mute" => Some(Behavior::Mute),
            "nothing" | "none" | "off" => Some(Behavior::Nothing),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Ducking {
    /// Default duck depth in dB (negative). Per-source `depth_db` overrides.
    pub depth_db: f32,
    /// A source counts as playing above this level (dBFS).
    pub threshold_db: f32,
    /// Level must stay above threshold this long before it ducks others.
    pub attack_ms: u32,
    /// Source stays "active" this long after dropping below threshold.
    pub hold_ms: u32,
    /// Time for a ducked source to return to full level.
    pub release_ms: u32,
    /// Time for a source to reach full duck depth.
    pub duck_ms: u32,
}

impl Default for Ducking {
    fn default() -> Self {
        Self { depth_db: -15.0, threshold_db: -50.0, attack_ms: 150, hold_ms: 1500, release_ms: 800, duck_ms: 150 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceConfig {
    pub name: String,
    #[serde(rename = "match")]
    pub rule: MatchRule,
    /// How far this source is ducked when a higher-priority one is active.
    #[serde(default)]
    pub depth_db: Option<f32>,
    /// What to do to this source when a higher-priority one is active (default: duck).
    #[serde(default)]
    pub behavior: Option<Behavior>,
}

/// `sources` order is priority order: index 0 wins.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub ducking: Ducking,
    #[serde(default, rename = "source")]
    pub sources: Vec<SourceConfig>,
}

impl Config {
    pub fn from_toml(s: &str) -> Result<Self, ConfigError> {
        let c: Config = toml::from_str(s)?;
        c.validate()?;
        Ok(c)
    }

    pub fn to_toml(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut seen = std::collections::HashSet::new();
        for s in &self.sources {
            if !seen.insert(&s.name) {
                return Err(ConfigError::DuplicateSource(s.name.clone()));
            }
            if let Some(d) = s.depth_db {
                if d > 0.0 {
                    return Err(ConfigError::BadDepth(s.name.clone(), d));
                }
            }
        }
        if self.ducking.depth_db > 0.0 {
            return Err(ConfigError::BadDepth("ducking".into(), self.ducking.depth_db));
        }
        Ok(())
    }

    pub fn behavior_for(&self, idx: usize) -> Behavior {
        self.sources[idx].behavior.unwrap_or_default()
    }

    pub fn depth_for(&self, idx: usize) -> f32 {
        self.sources[idx].depth_db.unwrap_or(self.ducking.depth_db)
    }

    /// Reorder sources to follow `order` (names). Unknown names are ignored;
    /// sources not mentioned keep their relative order at the end.
    pub fn set_priority(&mut self, order: &[String]) {
        let mut out = Vec::with_capacity(self.sources.len());
        for n in order {
            if let Some(p) = self.sources.iter().position(|s| &s.name == n) {
                out.push(self.sources.remove(p));
            }
        }
        out.append(&mut self.sources);
        self.sources = out;
    }

    /// Default config: local apps always on top.
    pub fn default_hub() -> Self {
        Config {
            ducking: Ducking::default(),
            sources: vec![SourceConfig {
                name: "Laptop".into(),
                rule: MatchRule { local_apps: true, ..Default::default() },
                depth_db: None,
                behavior: None,
            }],
        }
    }

    /// Add a source at the lowest priority. Name is made unique.
    pub fn add_source(&mut self, name: &str, rule: MatchRule) -> usize {
        let mut n = name.to_string();
        let mut i = 2;
        while self.sources.iter().any(|s| s.name == n) {
            n = format!("{name} {i}");
            i += 1;
        }
        self.sources.push(SourceConfig { name: n, rule, depth_db: None, behavior: None });
        self.sources.len() - 1
    }

    pub fn remove_source(&mut self, name: &str) -> bool {
        let before = self.sources.len();
        self.sources.retain(|s| s.name != name);
        self.sources.len() != before
    }

    /// Add a source just below `after` (or last). Name is made unique.
    pub fn insert_source_after(&mut self, after: Option<usize>, name: &str, rule: MatchRule) -> usize {
        let idx = self.add_source(name, rule);
        let at = after.map_or(idx, |a| (a + 1).min(idx));
        let s = self.sources.remove(idx);
        self.sources.insert(at, s);
        at
    }

    /// The source a node belongs to: the most specific matching rule wins
    /// (a program's own source beats the catch-all group), ties go to the
    /// higher priority.
    pub fn classify(&self, node: &crate::matching::NodeInfo) -> Option<usize> {
        self.sources
            .iter()
            .enumerate()
            .filter(|(_, s)| s.rule.matches(node))
            .max_by_key(|(i, s)| (s.rule.specificity(), std::cmp::Reverse(*i)))
            .map(|(i, _)| i)
    }
}
