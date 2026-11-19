//! Wire types shared by daemon, CLI, GUI and tray (JSON over D-Bus).

use serde::{Deserialize, Serialize};

pub const BUS_NAME: &str = "org.wavectl.Daemon";
pub const OBJECT_PATH: &str = "/org/wavectl/Daemon";

/// A media player (MPRIS app or phone over Bluetooth) matched to a stream.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PlayerView {
    /// Pass to the player_control D-Bus method.
    pub id: String,
    /// e.g. "Spotify".
    pub name: String,
    /// "playing" | "paused" | "stopped"
    pub status: String,
    pub title: String,
    pub artist: String,
    pub album: String,
}

/// One PipeWire stream (a program's playback, or a Bluetooth device's audio).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StreamView {
    /// PipeWire node id; use with SetStreamVolume/SetStreamMute.
    pub id: u32,
    /// Program or device name.
    pub name: String,
    /// What it is playing (tab title, track, ...), if known.
    pub detail: String,
    /// The user's own volume (linear, 1.0 = 100%), independent of ducking.
    pub volume: f32,
    pub muted: bool,
    /// Smoothed display level in dBFS (max of both channels).
    pub level_db: Option<f32>,
    /// Smoothed per-channel levels in dBFS (mono is shown on both).
    pub level_l: f32,
    pub level_r: f32,
    /// Node id of the output device this stream is currently linked to.
    pub sink_id: Option<u32>,
    /// Recording streams (other programs' inputs) this stream is also linked into.
    pub sends: Vec<u32>,
    /// The media player behind this stream, with now-playing info.
    pub player: Option<PlayerView>,
}

/// An output device a stream can be routed to.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OutputView {
    pub id: u32,
    pub name: String,
    pub is_default: bool,
}

/// A program that records audio, i.e. something another program can be sent into.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CaptureView {
    pub id: u32,
    pub name: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SourceView {
    pub name: String,
    /// "idle" | "pending" | "active" | "hold"
    pub state: String,
    pub active: bool,
    /// Duck currently applied (dB, <= 0).
    pub gain_db: f32,
    pub ducked: bool,
    /// Pre-duck level in dBFS, `None` when nothing is connected.
    pub level_db: Option<f32>,
    /// Effective depth for this source.
    pub depth_db: f32,
    /// True when `depth_db` is an explicit per-source override.
    pub depth_override: bool,
    /// "group" (all local programs) | "app" (one program) | "device" | "node".
    pub kind: String,
    /// "duck" | "pause" | "mute" | "nothing": what happens when a higher source plays.
    pub behavior: String,
    pub behavior_override: bool,
    /// A higher-priority source is playing right now.
    pub suppressed: bool,
    /// Pause behavior: player(s) we paused and will resume.
    pub paused: bool,
    /// Currently connected PipeWire node names.
    pub nodes: Vec<String>,
    pub streams: Vec<StreamView>,
    pub bluez_address: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StateView {
    pub enabled: bool,
    pub default_depth_db: f32,
    pub threshold_db: f32,
    /// Priority order, index 0 first.
    pub sources: Vec<SourceView>,
    /// True if any source is currently ducked.
    pub ducking: bool,
    pub outputs: Vec<OutputView>,
    pub captures: Vec<CaptureView>,
}
