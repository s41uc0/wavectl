//! Pure priority/ducking logic for wavectl. No I/O; fully unit-testable.

pub mod api;
pub mod config;
pub mod engine;
pub mod matching;

pub use config::{Behavior, Config, Ducking, SourceConfig};
pub use engine::{Engine, SourceState, SourceStatus};
pub use matching::{MatchRule, NodeInfo};
