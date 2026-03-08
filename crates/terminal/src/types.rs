use bytes::Bytes;
use pty::WindowSize;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::IO::PostQueuedCompletionStatus;

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
    Bell,
    TitleChanged(String),
    Exited(Option<ExitStatus>),
    Error(String),
}

/// Messages sent from GPUI main thread (and read thread for Reply) to IO thread
/// via crossbeam_channel.
pub enum IoMsg {
    /// User input bytes → WriteFile(conin).
    Input(Bytes),
    /// Device response bytes → WriteFile(conin).
    /// Same pipe, but semantically separate from user input.
    /// Dropped during shutdown (unlike Input which may be flushed).
    Reply(Bytes),
    /// Resize request. IO thread coalesces (25ms), then signals read thread.
    Resize(WindowSize),
    /// Begin/reset the 1-second synchronized output safety timer.
    StartSyncOutput,
    /// Ordered shutdown.
    Close,
}

/// Completion key used to signal the read thread via IOCP.
pub const READ_NOTIFY_KEY: usize = 1;

/// Lightweight signaling mechanism from IO thread → read thread.
/// Integrates with IOCP via PostQueuedCompletionStatus.
pub struct ReadThreadNotify {
    /// IOCP handle used to post wakeups to the read thread.
    pub iocp: HANDLE,
    /// Pending resize. Written by IO thread, read by read thread.
    /// Mutex is uncontended in practice — only touched on resize (rare).
    pub pending_resize: Mutex<Option<WindowSize>>,
    /// Shutdown flag.
    pub closing: AtomicBool,
    /// Serializes ResizePseudoConsole (read thread) vs ClosePseudoConsole (IO thread).
    pub hpcon_op: Mutex<()>,
}

// SAFETY: `iocp` is a Win32 IO completion port handle. It is only used via
// Win32 APIs (PostQueuedCompletionStatus, GetQueuedCompletionStatusEx), which
// are safe to call from any thread. The Mutex/AtomicBool fields are already
// Send+Sync. Sharing the HANDLE across threads is the normal Win32 pattern.
unsafe impl Send for ReadThreadNotify {}
unsafe impl Sync for ReadThreadNotify {}

impl ReadThreadNotify {
    pub fn new(iocp: HANDLE) -> Self {
        Self {
            iocp,
            pending_resize: Mutex::new(None),
            closing: AtomicBool::new(false),
            hpcon_op: Mutex::new(()),
        }
    }

    /// Store a pending resize (IO thread side).
    pub fn set_resize(&self, size: WindowSize) {
        *self.pending_resize.lock().unwrap() = Some(size);
    }

    /// Take pending resize (read thread side). Returns None if no resize pending.
    pub fn take_resize(&self) -> Option<WindowSize> {
        self.pending_resize.lock().unwrap().take()
    }

    /// Signal the read thread to wake and process resize/close.
    pub fn signal(&self) {
        // SAFETY: iocp is a valid IOCP handle for this session.
        unsafe {
            PostQueuedCompletionStatus(self.iocp, 0, READ_NOTIFY_KEY, std::ptr::null_mut());
        }
    }
}

impl Drop for ReadThreadNotify {
    fn drop(&mut self) {
        if !self.iocp.is_null() {
            unsafe {
                CloseHandle(self.iocp);
            }
            self.iocp = std::ptr::null_mut();
        }
    }
}
