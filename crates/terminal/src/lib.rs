mod config;
mod input;
mod io_thread;
mod platform;
mod session;
mod surface;
mod types;

pub use config::{RenderConfig, SpawnConfig};
pub use ghostty::ScrollbarInfo;
pub use session::{SessionEvent, TerminalSession};
pub use surface::AppAction;
pub use types::{
    IoEvent, ProcessState, RendererMessage, SessionId, SessionMetadata, TerminalDimensions,
};
pub use zconpty::{MouseButton, MousePosition};
