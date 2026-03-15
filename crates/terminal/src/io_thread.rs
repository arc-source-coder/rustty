/// IO thread — the cold-path writer/timer thread.
///
/// Mirrors Ghostty's `Thread.zig`. Owns `PtyWriter` (conin + HPCON for close),
/// handles overlapped writes to conin, coalesces resize requests (25ms), and
/// manages the synchronized-output safety timer (1s).
///
/// Driven by `crossbeam_channel::recv_timeout`; the timeout itself does
/// double duty as the timer mechanism, so no auxiliary timer thread is needed.
use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use std::{io, mem, ptr};

use async_channel::Sender;
use bytes::{Bytes, BytesMut};
use crossbeam_channel::{Receiver, RecvTimeoutError};

use ghostty_vt::Terminal;
use pty::{PtyWriter, WindowSize};

use crate::types::{IoMsg, ReadThreadNotify, RendererMessage, ScrollOp};

// Windows APIs
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, HANDLE};
use windows_sys::Win32::Storage::FileSystem::WriteFile;
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

/// Maximum bytes sent in a single WriteFile call.
/// Matches Windows Terminal's write chunk size.
const WRITE_CHUNK: usize = 64 * 1024;

/// Resize coalesce window — matches Ghostty's Thread.zig.
const RESIZE_COALESCE: Duration = Duration::from_millis(25);

/// Synchronized-output safety timeout — matches Ghostty's 1-second limit.
const SYNC_OUTPUT_TIMEOUT: Duration = Duration::from_secs(1);

/// Fallback recv_timeout when no timers are active.
/// Long enough to effectively block, short enough that we'll notice if
/// the channel is somehow disconnected without a Close message.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Poll interval used when a write is in-flight and write_buf has more data.
/// Keeps the queue draining without blocking for up to IDLE_TIMEOUT.
const WRITE_POLL: Duration = Duration::from_millis(5);

/// Spawn the IO thread. Returns the JoinHandle.
pub fn spawn(
    writer: PtyWriter,
    terminal: Arc<Mutex<Terminal>>,
    notify: Arc<ReadThreadNotify>,
    io_rx: Receiver<IoMsg>,
    signal_tx: Sender<()>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("pty-io".into())
        .spawn(move || {
            let mut thread = IoThread::new(writer, terminal, notify, signal_tx);
            thread.run(io_rx);
        })
        .expect("failed to spawn IO thread")
}

struct IoThread {
    writer: PtyWriter,
    terminal: Arc<Mutex<Terminal>>,
    notify: Arc<ReadThreadNotify>,
    signal_tx: Sender<()>,
    renderer_tx: Option<crossbeam_channel::Sender<RendererMessage>>,

    /// Bytes waiting to be written to conin.
    /// Each entry owns its backing buffer for the duration of the write.
    write_queue: VecDeque<WriteChunk>,

    /// Optional coalescing buffer used to batch small writes.
    coalesce_buf: Option<BytesMut>,

    /// Pool of reusable BytesMut buffers for coalescing.
    coalesce_pool: Vec<BytesMut>,

    /// OVERLAPPED struct for the in-flight write. Heap-pinned via Box so its
    /// address is stable across moves of IoThread.
    write_overlapped: Box<OVERLAPPED>,

    /// Whether a WriteFile is currently in-flight.
    write_pending: bool,

    /// When Some, a resize fires after this instant.
    resize_deadline: Option<Instant>,

    /// Latest resize request — earlier ones are discarded (last-wins).
    pending_resize: Option<WindowSize>,

    /// When Some, the sync-output safety timer fires after this instant.
    sync_output_deadline: Option<Instant>,
}

struct WriteChunk {
    bytes: Bytes,
    offset: usize,
}

impl IoThread {
    fn new(
        writer: PtyWriter,
        terminal: Arc<Mutex<Terminal>>,
        notify: Arc<ReadThreadNotify>,
        signal_tx: Sender<()>,
    ) -> Self {
        let mut overlapped = unsafe { mem::zeroed::<OVERLAPPED>() };
        // Use a manual-reset event for write completion so we can poll it
        // without consuming it if we choose not to wait.
        overlapped.hEvent = create_event();

        Self {
            writer,
            terminal,
            notify,
            signal_tx,
            renderer_tx: None,
            write_queue: VecDeque::new(),
            coalesce_buf: None,
            coalesce_pool: Vec::new(),
            write_overlapped: Box::new(overlapped),
            write_pending: false,
            resize_deadline: None,
            pending_resize: None,
            sync_output_deadline: None,
        }
    }

    fn run(&mut self, io_rx: Receiver<IoMsg>) {
        loop {
            let timeout = self.next_timer_deadline();
            match io_rx.recv_timeout(timeout) {
                Ok(IoMsg::InputInline { len, buf }) => {
                    self.enqueue_bytes(Bytes::copy_from_slice(&buf[..len as usize]));
                    self.flush_writes();
                }
                Ok(IoMsg::Input(bytes)) => {
                    self.enqueue_bytes(bytes);
                    self.flush_writes();
                }
                Ok(IoMsg::Reply(bytes)) => {
                    self.enqueue_bytes(bytes);
                    self.flush_writes();
                }
                Ok(IoMsg::Resize(size)) => {
                    // Last-wins coalescing.
                    self.pending_resize = Some(size);
                    self.resize_deadline = Some(Instant::now() + RESIZE_COALESCE);
                }
                Ok(IoMsg::Scroll(op)) => {
                    self.apply_scroll(op);
                }
                Ok(IoMsg::StartSyncOutput) => {
                    // Start or reset the 1-second safety timer.
                    self.sync_output_deadline = Some(Instant::now() + SYNC_OUTPUT_TIMEOUT);
                }
                Ok(IoMsg::AttachRenderer(sender)) => {
                    self.renderer_tx = Some(sender);
                }
                Ok(IoMsg::DetachRenderer) => {
                    self.renderer_tx = None;
                }
                Ok(IoMsg::Close) => {
                    self.handle_close(&io_rx);
                    break;
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.fire_timers();
                    // Flush any buffered writes after timer work (e.g., resize
                    // signal sent — now a good time to drain the write queue).
                    self.flush_writes();
                }
                Err(RecvTimeoutError::Disconnected) => {
                    // Sender side dropped without sending Close — treat as shutdown.
                    break;
                }
            }
        }
    }

    /// Compute the timeout for the next `recv_timeout` call.
    ///
    /// Returns the minimum time until any active deadline, or `IDLE_TIMEOUT`
    /// when no timers are armed. Saturates to zero rather than underflowing
    /// so we fire immediately for already-expired deadlines.
    ///
    /// When a write is in-flight and more data is queued, uses `WRITE_POLL`
    /// as the floor so the queue keeps draining without waiting up to IDLE_TIMEOUT.
    fn next_timer_deadline(&self) -> Duration {
        let now = Instant::now();
        // Note: coalesced bytes in `coalesce_buf` are not considered "queued" here.
        // If testing shows interactive latency regressions during in-flight writes,
        // consider treating non-empty `coalesce_buf` as queued to use WRITE_POLL.
        let mut min = if self.write_pending && !self.write_queue.is_empty() {
            WRITE_POLL
        } else {
            IDLE_TIMEOUT
        };

        if let Some(deadline) = self.resize_deadline {
            min = min.min(deadline.saturating_duration_since(now));
        }
        if let Some(deadline) = self.sync_output_deadline {
            min = min.min(deadline.saturating_duration_since(now));
        }
        min
    }

    fn fire_timers(&mut self) {
        let now = Instant::now();

        if let Some(deadline) = self.resize_deadline {
            if now >= deadline {
                self.resize_deadline = None;
                if let Some(size) = self.pending_resize.take() {
                    self.notify.set_resize(size);
                    self.notify.signal();
                    // Nudge the renderer thread after a committed resize.
                    // Failures are normal if the renderer is not yet attached or
                    // is shutting down.
                    if let Some(sender) = self.renderer_tx.as_ref() {
                        sender.try_send(RendererMessage::Wake).ok();
                    }
                }
            }
        }

        if let Some(deadline) = self.sync_output_deadline {
            if now >= deadline {
                self.sync_output_deadline = None;
                let mut term = self.terminal.lock().expect("terminal mutex poisoned");
                term.reset_synchronized_output();
                drop(term);
                self.signal_tx.try_send(()).ok();
            }
        }
    }

    /// Apply a scroll operation under the terminal mutex, then wake the renderer.
    ///
    /// Mirrors Ghostty's `Termio.scrollViewport`: the lock is acquired
    /// here on the IO thread, never on the UI thread.
    fn apply_scroll(&self, op: ScrollOp) {
        {
            let mut term = self.terminal.lock().expect("terminal mutex poisoned");
            match op {
                ScrollOp::Delta(delta) => term.scroll_viewport(delta),
                ScrollOp::Top => term.scroll_to_top(),
                ScrollOp::Bottom => term.scroll_to_bottom(),
            }
        }
        self.signal_tx.try_send(()).ok();
    }

    fn handle_close(&mut self, io_rx: &Receiver<IoMsg>) {
        // 1. Signal read thread to shut down FIRST — prevents new resizes/replies.
        self.notify.closing.store(true, Ordering::Release);

        // 2. Clear pending resize/timers — no more resizes after close.
        self.pending_resize = None;
        self.resize_deadline = None;
        self.sync_output_deadline = None;

        // 3. Signal read thread via IOCP.
        self.notify.signal();

        // 4. Drain remaining messages — flush user Input, discard the rest.
        while let Ok(msg) = io_rx.try_recv() {
            match msg {
                IoMsg::InputInline { len, buf } => {
                    self.enqueue_bytes(Bytes::copy_from_slice(&buf[..len as usize]));
                }
                IoMsg::Input(bytes) => {
                    self.enqueue_bytes(bytes);
                }
                IoMsg::Reply(_) => {}  // discard device responses after close
                IoMsg::Resize(_) => {} // discard
                IoMsg::Scroll(_) => {} // discard — viewport state is moot at shutdown
                IoMsg::StartSyncOutput => {} // discard
                IoMsg::AttachRenderer(_) => {} // discard
                IoMsg::DetachRenderer => {} // discard
                IoMsg::Close => {}     // duplicate
            }
        }

        // 5. Flush any buffered user input (best-effort).
        self.flush_writes();

        // 6. Serialize with any in-flight resize, then start async close.
        {
            let _guard = self.notify.hpcon_op.lock().unwrap();
            self.writer.backend.close_async();
        }
        // unlock hpcon_op — IO loop exits after this
    }

    fn enqueue_bytes(&mut self, bytes: Bytes) {
        if bytes.is_empty() {
            return;
        }
        if bytes.len() >= WRITE_CHUNK {
            self.flush_coalesce_into_queue();
            self.write_queue.push_back(WriteChunk { bytes, offset: 0 });
            return;
        }

        let mut buf = self.take_coalesce_buf();
        if buf.len() + bytes.len() > WRITE_CHUNK {
            self.write_queue.push_back(WriteChunk {
                bytes: buf.freeze(),
                offset: 0,
            });
            buf = self.take_coalesce_buf();
        }
        buf.extend_from_slice(&bytes);
        self.coalesce_buf = Some(buf);
    }

    fn take_coalesce_buf(&mut self) -> BytesMut {
        if let Some(buf) = self.coalesce_buf.take() {
            return buf;
        }
        if let Some(mut buf) = self.coalesce_pool.pop() {
            buf.clear();
            return buf;
        }
        BytesMut::with_capacity(WRITE_CHUNK)
    }

    fn flush_coalesce_into_queue(&mut self) {
        if let Some(buf) = self.coalesce_buf.take() {
            if buf.is_empty() {
                self.coalesce_pool.push(buf);
            } else {
                self.write_queue.push_back(WriteChunk {
                    bytes: buf.freeze(),
                    offset: 0,
                });
            }
        }
    }

    fn recycle_bytes(&mut self, bytes: Bytes) {
        if bytes.len() > WRITE_CHUNK {
            return;
        }
        if let Ok(mut buf) = bytes.try_into_mut() {
            if buf.capacity() <= WRITE_CHUNK * 2 {
                buf.clear();
                self.coalesce_pool.push(buf);
            }
        }
    }

    fn advance_front(&mut self, bytes_written: usize) {
        if bytes_written == 0 {
            return;
        }
        if let Some(front) = self.write_queue.front_mut() {
            front.offset = front.offset.saturating_add(bytes_written);
            if front.offset >= front.bytes.len() {
                let front = self.write_queue.pop_front().unwrap();
                self.recycle_bytes(front.bytes);
            }
        }
    }

    fn complete_inflight(&mut self) -> bool {
        if !self.write_pending {
            return true;
        }

        let conin = self.writer.conin.raw();
        let mut bytes_written: u32 = 0;
        let ok = unsafe {
            GetOverlappedResult(
                conin,
                self.write_overlapped.as_ref() as *const _ as *mut _,
                &mut bytes_written,
                0, // bWait = FALSE
            )
        };
        if ok != 0 {
            self.write_pending = false;
            self.advance_front(bytes_written as usize);
            return true;
        }

        let err = io::Error::last_os_error();
        let code = err.raw_os_error().unwrap_or(0) as u32;
        if code == ERROR_IO_INCOMPLETE {
            return false;
        }

        unsafe {
            CancelIoEx(conin, self.write_overlapped.as_ref() as *const _ as *mut _);
            let mut _n: u32 = 0;
            GetOverlappedResult(
                conin,
                self.write_overlapped.as_ref() as *const _ as *mut _,
                &mut _n,
                1, // bWait = TRUE
            );
        }

        self.write_pending = false;
        if let Some(front) = self.write_queue.pop_front() {
            self.recycle_bytes(front.bytes);
        }
        log::warn!("io_thread: write to conin failed: {err}");
        true
    }

    fn start_write(&mut self) -> bool {
        let front = match self.write_queue.front() {
            Some(front) => front,
            None => return false,
        };

        let remaining = front.bytes.len().saturating_sub(front.offset);
        if remaining == 0 {
            let front = self.write_queue.pop_front().unwrap();
            self.recycle_bytes(front.bytes);
            return true;
        }

        let to_send = remaining.min(WRITE_CHUNK);

        let overlapped = self.write_overlapped.as_mut();
        overlapped.Anonymous.Anonymous.Offset = 0;
        overlapped.Anonymous.Anonymous.OffsetHigh = 0;
        overlapped.Internal = 0;
        overlapped.InternalHigh = 0;

        let conin = self.writer.conin.raw();
        let mut bytes_written: u32 = 0;
        let ok = unsafe {
            WriteFile(
                conin,
                front.bytes.as_ptr().add(front.offset),
                to_send as u32,
                &mut bytes_written,
                overlapped,
            )
        };

        if ok != 0 {
            self.advance_front(bytes_written as usize);
            self.write_pending = false;
            return true;
        }

        let err = io::Error::last_os_error();
        let code = err.raw_os_error().unwrap_or(0) as u32;
        if code == ERROR_IO_PENDING {
            self.write_pending = true;
            return true;
        }

        if let Some(front) = self.write_queue.pop_front() {
            self.recycle_bytes(front.bytes);
        }
        self.write_pending = false;
        log::warn!("io_thread: WriteFile to conin failed: {err}");
        true
    }

    /// Write as much of `write_queue` to conin as possible using overlapped IO.
    ///
    /// If a write is already in-flight, we first attempt to complete it
    /// (polling, not blocking) to reclaim buffers. On any error we
    /// log and discard — writes to conin are best-effort (a dead shell means
    /// there's nothing to receive them anyway).
    fn flush_writes(&mut self) {
        loop {
            if !self.complete_inflight() {
                return;
            }

            if self.write_queue.is_empty() && !self.write_pending {
                self.flush_coalesce_into_queue();
            }

            if self.write_queue.is_empty() {
                return;
            }

            if !self.start_write() {
                return;
            }
        }
    }
}

impl Drop for IoThread {
    fn drop(&mut self) {
        // Cancel any in-flight write and wait for kernel to release OVERLAPPED.
        if self.write_pending {
            let conin = self.writer.conin.raw();
            unsafe {
                CancelIoEx(conin, self.write_overlapped.as_ref() as *const _ as *mut _);
                let mut _n: u32 = 0;
                GetOverlappedResult(
                    conin,
                    self.write_overlapped.as_ref() as *const _ as *mut _,
                    &mut _n,
                    1, // bWait = TRUE
                );
            }
            self.write_pending = false;
        }

        // Close the event handle we created in new().
        unsafe { CloseHandle(self.write_overlapped.hEvent) };
    }
}

/// Create a Win32 manual-reset event (initially non-signaled).
fn create_event() -> HANDLE {
    let handle = unsafe {
        windows_sys::Win32::System::Threading::CreateEventW(
            ptr::null(), // lpEventAttributes
            1,           // bManualReset = TRUE
            0,           // bInitialState = FALSE
            ptr::null(), // lpName (unnamed)
        )
    };
    assert!(
        !handle.is_null(),
        "CreateEventW failed for IO thread write event"
    );
    handle
}
