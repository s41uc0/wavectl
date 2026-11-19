use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Properties of a PipeWire node relevant to classification.
#[derive(Debug, Clone, Default)]
pub struct NodeInfo {
    pub node_name: String,
    /// `api.bluez5.address`, if a Bluetooth node.
    pub bluez_address: Option<String>,
    /// True for application playback streams (`Stream/Output/Audio`).
    pub is_app_stream: bool,
    pub props: HashMap<String, String>,
}

/// All set fields must match (AND). Empty rule matches nothing.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MatchRule {
    /// Local application playback streams that are not Bluetooth.
    pub local_apps: bool,
    /// Glob (`*` wildcard) on `node.name`.
    pub node_name: Option<String>,
    /// Bluetooth MAC, case-insensitive.
    pub bluez_address: Option<String>,
    /// `application.name` of the program, case-insensitive.
    pub app_name: Option<String>,
}

impl MatchRule {
    /// How specific the rule is: the catch-all "local apps" group is 0, anything
    /// that names a program, node or device is higher. More specific wins.
    pub fn specificity(&self) -> usize {
        [self.node_name.is_some(), self.bluez_address.is_some(), self.app_name.is_some()].iter().filter(|b| **b).count()
    }

    /// What kind of source this is, for display: "group" | "app" | "device" | "node".
    pub fn kind(&self) -> &'static str {
        if self.bluez_address.is_some() {
            "device"
        } else if self.app_name.is_some() {
            "app"
        } else if self.node_name.is_some() {
            "node"
        } else {
            "group"
        }
    }

    pub fn matches(&self, n: &NodeInfo) -> bool {
        if !self.local_apps && self.node_name.is_none() && self.bluez_address.is_none() && self.app_name.is_none() {
            return false;
        }
        if self.local_apps && !(n.is_app_stream && n.bluez_address.is_none()) {
            return false;
        }
        if let Some(g) = &self.node_name {
            if !glob(g, &n.node_name) {
                return false;
            }
        }
        if let Some(a) = &self.app_name {
            match n.props.get("application.name") {
                Some(b) if b.eq_ignore_ascii_case(a) => {}
                _ => return false,
            }
        }
        if let Some(a) = &self.bluez_address {
            match &n.bluez_address {
                Some(b) if b.eq_ignore_ascii_case(a) => {}
                _ => return false,
            }
        }
        true
    }
}

/// Minimal `*` glob.
pub fn glob(pat: &str, text: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == text;
    }
    let mut rest = text;
    for (i, p) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(p) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(p);
        } else {
            match rest.find(p) {
                Some(pos) => rest = &rest[pos + p.len()..],
                None => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globbing() {
        assert!(glob("a*c", "abc"));
        assert!(glob("*", "x"));
        assert!(glob("alsa_input.*capture*", "alsa_input.usb-x.capture.0"));
        assert!(!glob("a*c", "abd"));
        assert!(!glob("abc", "abcd"));
    }

    #[test]
    fn rules() {
        let bt = NodeInfo { node_name: "bluez_input.AA".into(), bluez_address: Some("AA:BB".into()), is_app_stream: true, ..Default::default() };
        let app = NodeInfo { node_name: "Firefox".into(), is_app_stream: true, ..Default::default() };
        let local = MatchRule { local_apps: true, ..Default::default() };
        assert!(local.matches(&app));
        assert!(!local.matches(&bt));
        let phone = MatchRule { bluez_address: Some("aa:bb".into()), ..Default::default() };
        assert!(phone.matches(&bt));
        assert!(!phone.matches(&app));
        assert!(!MatchRule::default().matches(&app));
        let mut ff = app.clone();
        ff.props.insert("application.name".into(), "Firefox".into());
        let by_app = MatchRule { app_name: Some("firefox".into()), ..Default::default() };
        assert!(by_app.matches(&ff));
        assert!(!by_app.matches(&app));
        assert!(by_app.specificity() > local.specificity());
    }
}
