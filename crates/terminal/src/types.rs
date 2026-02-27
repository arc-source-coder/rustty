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

    pub fn as_u64(self) -> u64 {
        self.0
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

/// Events sent from the IO thread to the UI thread via bounded channel.
/// Mirrors Ghostty's surface_mailbox pattern (SPSC, capacity 64).
#[derive(Debug)]
pub enum IoEvent {
    SideEffect(SideEffect),
    Exited(Option<ExitStatus>),
    Error(String),
}

/// Parameters for resizing the terminal inside a single lock scope.
/// Used by `RenderSnapshot::capture()` to fold set_cell_size + resize +
/// snapshot into one mutex acquisition (instead of 3 separate locks).
#[derive(Debug, Clone, Copy)]
pub struct ResizeRequest {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}
