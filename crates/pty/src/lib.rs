use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitStatus;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use self::unix::Pty;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::Pty;

mod handle;
pub use handle::PtyHandle;

// --- Shared types used by both platform backends ---

#[derive(Debug, PartialEq, Eq)]
pub enum ChildEvent {
    Exited(Option<ExitStatus>),
}

#[derive(Clone, Copy, Debug)]
pub struct WindowSize {
    pub num_lines: u16,
    pub num_cols: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

#[derive(Clone, Debug, Default)]
pub struct Shell {
    pub program: String,
    pub args: Vec<String>,
}

impl Shell {
    pub fn new(program: String, args: Vec<String>) -> Self {
        Self { program, args }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub shell: Option<Shell>,
    pub working_directory: Option<PathBuf>,
    pub env: HashMap<String, String>,
    #[cfg(windows)]
    pub escape_args: bool,
}

// --- Channel protocol ---

pub const PTY_EVENT_CHANNEL_CAPACITY: usize = 64;
pub const PTY_COMMAND_CHANNEL_CAPACITY: usize = 64;

pub enum PtyCommand {
    Write(Vec<u8>),
    Resize(WindowSize),
    Close,
}

pub enum PtyEvent {
    Output(Vec<u8>),
    Exited(Option<ExitStatus>),
    Error(std::io::Error),
}

/// Create a new platform PTY.
pub fn new(config: &Options, window_size: WindowSize) -> std::io::Result<Pty> {
    #[cfg(windows)]
    {
        windows::new(config, window_size)
    }
    #[cfg(unix)]
    {
        unix::new(config, window_size)
    }
}
