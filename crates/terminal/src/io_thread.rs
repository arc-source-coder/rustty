/// IO thread — cold path for input/timers/writer state.
///
/// Responsibilities:
/// - drain `IoMsg` mailbox,
/// - coalesce resizes and signal read thread,
/// - enforce synchronized-output safety timeout,
/// - write input to `conin` via `NtWriteFile` APC completions.
use std::collections::VecDeque;
use std::io;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_channel::Sender;
use bytes::{Bytes, BytesMut};
use ghostty_vt::Terminal;
use pty::{PtyWriter, WindowSize};

use crate::platform::windows::io::{AsyncIo, alertable_wait, async_write};
use crate::platform::windows::ntdll::{
    STATUS_ALERTED, STATUS_CANCELLED, STATUS_END_OF_FILE, STATUS_PENDING, STATUS_PIPE_BROKEN,
    STATUS_SUCCESS, STATUS_TIMEOUT, STATUS_USER_APC,
};
use crate::platform::windows::thread::{PlatformThread, set_current_thread_name};
use crate::types::{IoMsg, IoThreadNotify, ReadThreadNotify, RendererMessage, ScrollOp};

const WRITE_CHUNK: usize = 64 * 1024;
const RESIZE_COALESCE: Duration = Duration::from_millis(25);
const SYNC_OUTPUT_TIMEOUT: Duration = Duration::from_secs(1);

/// Context passed through `NtCreateThreadEx` start routine.
struct IoThreadContext {
    writer: PtyWriter,
    terminal: Arc<Mutex<Terminal>>,
    read_notify: Arc<ReadThreadNotify>,
    io_notify: Arc<IoThreadNotify>,
    signal_tx: Sender<()>,
}

pub fn spawn_suspended(
    writer: PtyWriter,
    terminal: Arc<Mutex<Terminal>>,
    read_notify: Arc<ReadThreadNotify>,
    io_notify: Arc<IoThreadNotify>,
    signal_tx: Sender<()>,
) -> io::Result<PlatformThread> {
    let ctx = Box::new(IoThreadContext {
        writer,
        terminal,
        read_notify,
        io_notify,
        signal_tx,
    });
    let ctx_ptr = Box::into_raw(ctx) as *mut std::ffi::c_void;
    match PlatformThread::spawn_suspended(io_thread_entry, ctx_ptr) {
        Ok(thread) => Ok(thread),
        Err(err) => {
            // SAFETY: ctx_ptr was produced by Box::into_raw above.
            unsafe {
                drop(Box::from_raw(ctx_ptr as *mut IoThreadContext));
            }
            Err(err)
        }
    }
}

/// Ntdll thread entry trampoline.
unsafe extern "system" fn io_thread_entry(context: *mut std::ffi::c_void) -> u32 {
    set_current_thread_name("pty-io");
    // SAFETY: context comes from Box::into_raw in spawn_suspended.
    let ctx = unsafe { Box::from_raw(context as *mut IoThreadContext) };
    let mut thread = IoThread::new(
        ctx.writer,
        ctx.terminal,
        ctx.read_notify,
        ctx.io_notify,
        ctx.signal_tx,
    );
    thread.run();
    0
}

/// Stateful IO worker.
struct IoThread {
    writer: PtyWriter,
    terminal: Arc<Mutex<Terminal>>,
    read_notify: Arc<ReadThreadNotify>,
    io_notify: Arc<IoThreadNotify>,
    signal_tx: Sender<()>,
    renderer_tx: Option<crossbeam_channel::Sender<RendererMessage>>,

    /// Bytes waiting to be written to conin.
    /// Each entry owns its backing buffer for the duration of the write.
    write_queue: VecDeque<WriteChunk>,
    /// Coalescing buffer used to batch small writes.
    coalesce_buf: BytesMut,
    /// Pool of reusable BytesMut buffers for coalescing.
    coalesce_pool: Vec<BytesMut>,

    write_io: AsyncIo,
    /// Whether a WriteFile is currently in-flight.
    write_pending: bool,

    /// When Some, a resize fires after this instant.
    resize_deadline: Option<Instant>,
    /// Latest resize request — earlier ones are discarded (last-wins).
    pending_resize: Option<WindowSize>,
    /// When Some, the sync-output safety timer fires after this instant.
    sync_output_deadline: Option<Instant>,
}

/// Owned write chunk with current progress offset.
struct WriteChunk {
    bytes: Bytes,
    offset: usize,
}

impl IoThread {
    fn new(
        writer: PtyWriter,
        terminal: Arc<Mutex<Terminal>>,
        read_notify: Arc<ReadThreadNotify>,
        io_notify: Arc<IoThreadNotify>,
        signal_tx: Sender<()>,
    ) -> Self {
        Self {
            writer,
            terminal,
            read_notify,
            io_notify,
            signal_tx,
            renderer_tx: None,
            write_queue: VecDeque::with_capacity(8),
            coalesce_buf: BytesMut::with_capacity(WRITE_CHUNK),
            coalesce_pool: Vec::new(),
            write_io: AsyncIo::new(),
            write_pending: false,
            resize_deadline: None,
            pending_resize: None,
            sync_output_deadline: None,
        }
    }

    /// Queue-driven IO loop.
    ///
    /// Uses one alertable wait primitive to unify timer wakeups,
    /// mailbox wakeups (`NtAlertThread`), and write APC completions.
    fn run(&mut self) {
        loop {
            while let Some(msg) = self.io_notify.queue.pop() {
                if self.handle_msg(msg) {
                    return;
                }
            }

            self.io_notify.wake_armed.store(false, Ordering::Release);

            if let Some(msg) = self.io_notify.queue.pop() {
                if self.handle_msg(msg) {
                    return;
                }
                while let Some(msg) = self.io_notify.queue.pop() {
                    if self.handle_msg(msg) {
                        return;
                    }
                }
                continue;
            }

            self.fire_timers();
            self.flush_writes();

            let wake = alertable_wait(self.next_timer_timeout_100ns());
            if wake != STATUS_SUCCESS
                && wake != STATUS_TIMEOUT
                && wake != STATUS_ALERTED
                && wake != STATUS_USER_APC
            {
                log::warn!("io_thread: unexpected NtDelayExecution status=0x{wake:08X}");
            }
        }
    }

    fn handle_msg(&mut self, msg: IoMsg) -> bool {
        match msg {
            IoMsg::InputInline { len, buf } => {
                self.enqueue_inline(&buf[..len as usize]);
            }
            IoMsg::Input(bytes) => {
                self.enqueue_bytes(bytes);
            }
            IoMsg::Reply(bytes) => {
                self.enqueue_bytes(bytes);
            }
            IoMsg::Resize(size) => {
                self.pending_resize = Some(size);
                self.resize_deadline = Some(Instant::now() + RESIZE_COALESCE);
            }
            IoMsg::Scroll(op) => {
                self.apply_scroll(op);
            }
            IoMsg::StartSyncOutput => {
                self.sync_output_deadline = Some(Instant::now() + SYNC_OUTPUT_TIMEOUT);
            }
            IoMsg::AttachRenderer(sender) => {
                self.renderer_tx = Some(sender);
            }
            IoMsg::DetachRenderer => {
                self.renderer_tx = None;
            }
            IoMsg::Close => {
                self.handle_close();
                return true;
            }
        }
        false
    }

    fn next_timer_timeout_100ns(&self) -> i64 {
        let deadline = match (self.resize_deadline, self.sync_output_deadline) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => return i64::MIN,
        };
        let d = deadline.saturating_duration_since(Instant::now());
        let ticks = d
            .as_secs()
            .saturating_mul(10_000_000)
            .saturating_add((d.subsec_nanos() / 100) as u64)
            .min(i64::MAX as u64) as i64;
        if ticks == 0 { 0 } else { -ticks }
    }

    fn fire_timers(&mut self) {
        let now = Instant::now();

        if let Some(deadline) = self.resize_deadline
            && now >= deadline
        {
            self.resize_deadline = None;
            if let Some(size) = self.pending_resize.take() {
                self.read_notify.set_resize(size);
                self.read_notify.signal();
                if let Some(sender) = self.renderer_tx.as_ref() {
                    sender.try_send(RendererMessage::Wake).ok();
                }
            }
        }

        if let Some(deadline) = self.sync_output_deadline
            && now >= deadline
        {
            self.sync_output_deadline = None;
            {
                let mut term = self.terminal.lock().expect("terminal mutex poisoned");
                term.reset_synchronized_output();
            }
            self.signal_tx.try_send(()).ok();
        }
    }

    /// Apply viewport scroll under terminal mutex.
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

    /// Ordered shutdown:
    /// - mark closing, wake read thread,
    /// - drain mailbox (flush input only),
    /// - flush outstanding writes,
    /// - close HPCON under resize/close lock.
    fn handle_close(&mut self) {
        self.read_notify.closing.store(true, Ordering::Release);
        self.pending_resize = None;
        self.resize_deadline = None;
        self.sync_output_deadline = None;
        self.read_notify.signal();

        while let Some(msg) = self.io_notify.queue.pop() {
            match msg {
                IoMsg::InputInline { len, buf } => {
                    self.enqueue_inline(&buf[..len as usize]);
                }
                IoMsg::Input(bytes) => self.enqueue_bytes(bytes),
                IoMsg::Reply(_) => {}
                IoMsg::Resize(_) => {}
                IoMsg::Scroll(_) => {}
                IoMsg::StartSyncOutput => {}
                IoMsg::AttachRenderer(_) => {}
                IoMsg::DetachRenderer => {}
                IoMsg::Close => {}
            }
        }

        // Best-effort flush only: do not block shutdown on conin completion.
        self.flush_writes();

        {
            let _guard = self.read_notify.hpcon_op.lock().unwrap();
            self.writer.backend.close_async();
        }
        // unlock hpcon_op — IO loop exits after this
    }

    /// Queue bytes for write coalescing/chunking.
    fn enqueue_bytes(&mut self, bytes: Bytes) {
        if bytes.is_empty() {
            return;
        }
        if bytes.len() >= WRITE_CHUNK {
            self.flush_coalesce_into_queue();
            self.write_queue.push_back(WriteChunk { bytes, offset: 0 });
            return;
        }
        self.append_small(&bytes);
    }

    /// Queue tiny inline bytes without allocating a `Bytes` owner.
    fn enqueue_inline(&mut self, bytes: &[u8]) {
        self.append_small(bytes);
    }

    /// Append a small slice to the coalesce buffer, flushing first if full.
    fn append_small(&mut self, src: &[u8]) {
        if src.is_empty() {
            return;
        }
        if self.coalesce_buf.len() + src.len() > WRITE_CHUNK {
            self.flush_coalesce_into_queue();
        }
        self.coalesce_buf.extend_from_slice(src);
    }

    /// Move coalesced bytes into the write queue.
    fn flush_coalesce_into_queue(&mut self) {
        if self.coalesce_buf.is_empty() {
            return;
        }
        let replacement = self.take_pool_buf();
        let full = std::mem::replace(&mut self.coalesce_buf, replacement);
        self.write_queue.push_back(WriteChunk {
            bytes: full.freeze(),
            offset: 0,
        });
    }

    /// Take a buffer from the recycle pool (or allocate a fresh one).
    fn take_pool_buf(&mut self) -> BytesMut {
        if let Some(mut buf) = self.coalesce_pool.pop() {
            buf.clear();
            buf
        } else {
            BytesMut::with_capacity(WRITE_CHUNK)
        }
    }

    /// Recycle small write buffers back to local pool.
    fn recycle_bytes(&mut self, bytes: Bytes) {
        if let Ok(mut buf) = bytes.try_into_mut()
            && buf.capacity() <= WRITE_CHUNK * 2
        {
            buf.clear();
            self.coalesce_pool.push(buf);
        }
    }

    /// Pop and recycle the front chunk.
    fn retire_front(&mut self) {
        if let Some(front) = self.write_queue.pop_front() {
            self.recycle_bytes(front.bytes);
        }
    }

    /// Advance front queued chunk by bytes written.
    fn advance_front(&mut self, bytes_written: usize) {
        if let Some(front) = self.write_queue.front_mut() {
            front.offset += bytes_written;
            if front.offset >= front.bytes.len() {
                self.retire_front();
            }
        }
    }

    /// Consume one completed in-flight write if APC flagged done.
    fn complete_inflight(&mut self) {
        if !self.write_pending || !self.write_io.done {
            return;
        }

        let status = self.write_io.iosb.status();
        let bytes_written = self.write_io.iosb.information;
        self.write_pending = false;

        if status == STATUS_SUCCESS && bytes_written > 0 {
            self.advance_front(bytes_written);
            return;
        }

        if status != STATUS_SUCCESS
            && status != STATUS_CANCELLED
            && status != STATUS_END_OF_FILE
            && status != STATUS_PIPE_BROKEN
        {
            log::warn!("io_thread: NtWriteFile completion failed: 0x{status:08X}");
        }
        self.retire_front();
    }

    /// Issue next write when no write is currently in-flight.
    fn flush_writes(&mut self) {
        self.complete_inflight();
        if self.write_pending {
            return;
        }

        self.flush_coalesce_into_queue();
        let (ptr, len) = match self.write_queue.front() {
            Some(front) => {
                let remaining = front.bytes.len() - front.offset;
                if remaining == 0 {
                    self.retire_front();
                    return;
                }
                let to_send = remaining.min(WRITE_CHUNK);
                (unsafe { front.bytes.as_ptr().add(front.offset) }, to_send)
            }
            None => return,
        };

        let status =
            unsafe { async_write(self.writer.conin.raw(), &mut self.write_io, ptr, len as u32) };
        match status {
            STATUS_SUCCESS | STATUS_PENDING => {
                self.write_pending = true;
            }
            _ => {
                log::warn!("io_thread: NtWriteFile failed: 0x{status:08X}");
                self.retire_front();
            }
        }
    }
}
