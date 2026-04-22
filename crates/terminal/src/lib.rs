mod config;
mod input;
mod io_thread;
mod platform;
mod session;
mod surface;
mod types;

pub use config::{RenderConfig, SpawnConfig};
pub use ghostty::ScrollbarInfo;
pub use session::TerminalSession;
pub use surface::AppAction;
pub use types::{GridSize, IoEvent, ProcessState, RendererMessage, SessionId, SessionMetadata};
pub use zconpty::{MouseButton, MousePosition};
