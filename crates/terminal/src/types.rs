use bytes::Bytes;
use crossbeam_queue::ArrayQueue;
use pty::WindowSize;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

use crate::platform::windows::io::sleep_100ns;
use crate::platform::windows::ntdll::{Handle, NtAlertThread};

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

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
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

pub const INLINE_IO_BYTES_CAPACITY: usize = 128;

/// Events sent from the IO thread to the UI thread via bounded channel.
/// Mirrors Ghostty's surface_mailbox pattern (SPSC, capacity 64).
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

/// Messages sent from GPUI main thread (and read thread for Reply) to IO thread.
pub enum IoMsg {
    /// Small user input bytes stored inline in the channel message.
    InputInline {
        len: u8,
        buf: [u8; INLINE_IO_BYTES_CAPACITY],
    },
    /// User input bytes → WriteFile(conin).
    Input(Bytes),
    /// Device response bytes → WriteFile(conin).
    /// Same pipe, but semantically separate from user input.
    /// Dropped during shutdown (unlike Input which may be flushed).
    Reply(Bytes),
    /// Resize request. IO thread coalesces (25ms), then signals read thread.
    Resize(WindowSize),
    /// Scroll the viewport. IO thread acquires terminal mutex and applies.
    Scroll(ScrollOp),
    /// Begin/reset the 1-second synchronized output safety timer.
    StartSyncOutput,
    /// Attach a renderer sender for forwarding committed resizes.
    AttachRenderer(crossbeam_channel::Sender<RendererMessage>),
    /// Detach renderer sender. Normal during view teardown.
    DetachRenderer,
    /// Ordered shutdown.
    Close,
}

/// Lightweight signaling mechanism from IO thread → read thread.
pub struct ReadThreadNotify {
    /// Read thread handle (set once before thread resume).
    read_thread: AtomicPtr<std::ffi::c_void>,
    /// Pending resize. Written by IO thread, read by read thread.
    pub pending_resize: Mutex<Option<WindowSize>>,
    /// Shutdown flag.
    pub closing: AtomicBool,
    /// Serializes ResizePseudoConsole (read thread) vs ClosePseudoConsole (IO thread).
    pub hpcon_op: Mutex<()>,
}

unsafe impl Send for ReadThreadNotify {}
unsafe impl Sync for ReadThreadNotify {}

impl ReadThreadNotify {
    pub fn new() -> Self {
        Self {
            read_thread: AtomicPtr::new(std::ptr::null_mut()),
            pending_resize: Mutex::new(None),
            closing: AtomicBool::new(false),
            hpcon_op: Mutex::new(()),
        }
    }

    pub fn set_read_thread(&self, handle: Handle) {
        self.read_thread.store(handle, Ordering::Release);
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
        let thread = self.read_thread.load(Ordering::Acquire) as Handle;
        if !thread.is_null() {
            unsafe {
                NtAlertThread(thread);
            }
        }
    }
}

pub struct IoThreadNotify {
    /// IO thread handle (set once before thread resume).
    io_thread: AtomicPtr<std::ffi::c_void>,
    /// Lock-free bounded mailbox.
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
    pub fn try_send(&self, msg: IoMsg) -> bool {
        match self.queue.push(msg) {
            Ok(()) => {
                self.alert_io_thread();
                true
            }
            Err(_) => false,
        }
    }

    /// Best-effort input send with inline fast path for tiny payloads.
    pub fn try_send_input_small(&self, data: &[u8]) {
        if data.len() > INLINE_IO_BYTES_CAPACITY {
            let _ = self.try_send(IoMsg::Input(Bytes::copy_from_slice(data)));
            return;
        }

        let mut buf = [0u8; INLINE_IO_BYTES_CAPACITY];
        buf[..data.len()].copy_from_slice(data);
        let _ = self.try_send(IoMsg::InputInline {
            len: data.len() as u8,
            buf,
        });
    }
}

/// Messages sent from the terminal/IO path to the renderer thread.
///
/// Defined in `terminal` so that `TerminalSession` can store a sender without
/// a circular dependency on the `renderer` crate. The `renderer` crate
/// re-exports this type.
///
/// Reserve a `DeviceLost` variant for the next slice — the renderer thread
/// must not outlive GPUI device recreation.
#[derive(Debug)]
pub enum RendererMessage {
    /// A new terminal frame is ready; renderer should re-record and publish.
    Wake,
    /// Ordered shutdown; renderer thread should exit its loop.
    Quit,
}
