//! ui-unabhängiger zustand, konfiguration und pipeline-orchestrierung. `core` linkt nie gegen appkit. die app reicht ihm jobs rein und bekommt events auf einem worker-thread zurück.

pub mod config;
pub mod log;
pub mod mode;
pub mod paths;
pub mod prompt;
pub mod session;
pub mod text;
pub mod trigger;

pub use config::{Config, HistoryLevel};
pub use mode::{Backend, Mode, ModeId, Refine};
pub use session::{Core, Event, Failure, Job, SpeakerEdit, Take};
pub use text::Vocabulary;
pub use trigger::{Action, HotkeyEvent, Input};
