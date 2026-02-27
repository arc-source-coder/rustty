use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, Ordering};

// Stable Identity
fn next_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SessionId(u64);

impl SessionId {
    pub fn new() -> Self {
        Self(next_id())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
}

impl GridSize {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }
}

#[derive(Debug)]
pub enum ProcessState {
    Running,
    Exited(Option<ExitStatus>),
    Error(String),
}

#[derive(Debug, Clone)]
pub enum SideEffect {
    Bell,
    TitleChanged(String),
}

#[derive(Debug, Default)]
pub struct SessionMetadata {
    pub title: Option<String>,
    pub cwd: Option<PathBuf>,
    pub bell_count: u32,
    pub has_unread_output: bool,
}
