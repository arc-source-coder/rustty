use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitStatus;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::{Pty, PtyReader, PtyWriter, ResizePseudoConsoleFn};

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

/// Create a new platform PTY.
pub fn new(config: &Options, window_size: WindowSize) -> std::io::Result<Pty> {
    windows::new(config, window_size)
}
