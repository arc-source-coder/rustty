use crossbeam_queue::ArrayQueue;
use ghostty::TerminalDimensions;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Arc, Mutex};
use zconpty::{KeyEvent, MouseEvent};

use crate::platform::windows::io::sleep_100ns;
use crate::platform::windows::ntdll::{Handle, NtAlertThread};

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

/// Events sent from the IO thread to the UI thread.
#[derive(Debug)]
pub enum IoEvent {
    Bell,
    TitleChanged(String),
    Exited(Option<ExitStatus>),
    Error(String),
}

/// Scroll operation forwarded from the UI thread to the IO thread.
/// Mirrors Ghostty's `terminal.Terminal.ScrollViewport` union.
/// The IO thread acquires the terminal mutex, applies the
/// scroll, then wakes the renderer.
pub enum ScrollOp {
    /// Scroll by delta rows. Negative = up (towards history).
    Delta(i32),
    /// Scroll to the top of scrollback.
    Top,
    /// Scroll to the bottom (active area).
    Bottom,
}

/// Typed input handed from the UI layer to the IO thread.
#[derive(Debug)]
pub enum IoInput {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Focus(bool),
    Text(Vec<u8>),
    // NOTE(renderer-refactor): This could be a slice
    Paste(Vec<u8>),
}

/// Messages sent from GPUI main thread to IO thread.
pub enum IoMsg {
    /// Lossless session input delivery. The IO thread is the single Rust-side
    /// ingress thread that calls into zconpty.
    Input(IoInput),
    /// Resize request. IO thread coalesces (25ms), then signals read thread.
    Resize(TerminalDimensions),
    /// Scroll the viewport. IO thread acquires terminal mutex and applies.
    Scroll(ScrollOp),
    /// Begin/reset the 1-second synchronized output safety timer.
    /// NOTE(renderer-refactor): Audit this.
    #[allow(dead_code)]
    StartSyncOutput,
    /// Ordered shutdown.
    Close,
}

// TODO: Courier
pub struct IoThreadNotify {
    /// IO thread handle (set once before thread resume).
    io_thread: AtomicPtr<std::ffi::c_void>,
    /// Lock-free bounded queue.
    pub queue: Arc<ArrayQueue<IoMsg>>,
    /// Wakeup coalescing guard.
    pub wake_armed: AtomicBool,
}

unsafe impl Send for IoThreadNotify {}
unsafe impl Sync for IoThreadNotify {}

impl IoThreadNotify {
    pub fn new(queue: Arc<ArrayQueue<IoMsg>>) -> Self {
        Self {
            io_thread: AtomicPtr::new(std::ptr::null_mut()),
            queue,
            wake_armed: AtomicBool::new(false),
        }
    }

    #[inline]
    pub fn set_io_thread(&self, handle: Handle) {
        self.io_thread.store(handle, Ordering::Release);
    }

    fn alert_io_thread(&self) {
        let thread = self.io_thread.load(Ordering::Acquire) as Handle;
        if !thread.is_null()
            && self
                .wake_armed
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            unsafe {
                NtAlertThread(thread);
            }
        }
    }

    /// Lossless path for protocol-critical messages.
    pub fn send_lossless(&self, msg: IoMsg) {
        let mut msg = msg;
        loop {
            match self.queue.push(msg) {
                Ok(()) => {
                    self.alert_io_thread();
                    return;
                }
                Err(returned) => {
                    msg = returned;
                    sleep_100ns(-1_000);
                }
            }
        }
    }

    /// Best-effort enqueue for UI-driven traffic.
    ///
    /// Returns `true` if enqueued, `false` when queue is full.
    #[inline]
    pub fn try_send(&self, msg: IoMsg) -> bool {
        match self.queue.push(msg) {
            Ok(()) => {
                self.alert_io_thread();
                true
            }
            Err(_) => false,
        }
    }
}

/// Direct wake path into the renderer thread.
///
/// This is shared by Ghostty's output callback and by Rust-side terminal
/// mutators that change render state without producing output.
///
/// TODO: Update doc comments
// TODO: Courier
pub struct RendererWake {
    sender: Mutex<Option<crossbeam_channel::Sender<()>>>,
}

impl RendererWake {
    pub fn new() -> Self {
        Self {
            sender: Mutex::new(None),
        }
    }

    #[inline]
    pub fn bind(&self, sender: crossbeam_channel::Sender<()>) {
        let mut slot = self.sender.lock().expect("renderer wake mutex poisoned");
        *slot = Some(sender);
    }

    #[inline]
    pub fn wake(&self) {
        let slot = self.sender.lock().expect("renderer wake mutex poisoned");
        if let Some(sender) = slot.as_ref() {
            let _ = sender.try_send(());
        }
    }
}
