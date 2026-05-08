/// IO thread — handles input ingress, mailbox, and timers.
use std::io;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use ghostty::Terminal;
use zconpty::ConPTY;

use crate::platform::windows::io::alertable_wait;
use crate::platform::windows::ntdll::{
    STATUS_ALERTED, STATUS_SUCCESS, STATUS_TIMEOUT, STATUS_USER_APC,
};
use crate::platform::windows::thread::{PlatformThread, set_current_thread_name};
use crate::types::{IoInput, IoMsg, IoThreadNotify, RendererWake, ScrollOp, TerminalDimensions};

const RESIZE_COALESCE: Duration = Duration::from_millis(25);
const SYNC_OUTPUT_TIMEOUT: Duration = Duration::from_secs(1);

fn resize_deadline_after(current: Option<Instant>, now: Instant) -> Instant {
    current.unwrap_or(now + RESIZE_COALESCE)
}

struct IoThreadContext {
    console_session: Arc<ConPTY>,
    terminal: Arc<Terminal>,
    io_notify: Arc<IoThreadNotify>,
    renderer_wake: Arc<RendererWake>,
}

pub fn spawn_suspended(
    console_session: Arc<ConPTY>,
    terminal: Arc<Terminal>,
    io_notify: Arc<IoThreadNotify>,
    renderer_wake: Arc<RendererWake>,
) -> io::Result<PlatformThread> {
    let ctx = Box::new(IoThreadContext {
        console_session,
        terminal,
        io_notify,
        renderer_wake,
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
    #[cfg(feature = "profiler")]
    tracy_client::set_thread_name!("pty-io");

    // SAFETY: context comes from Box::into_raw in spawn_suspended.
    let ctx = unsafe { Box::from_raw(context as *mut IoThreadContext) };
    let mut thread = IoThread::new(
        ctx.console_session,
        ctx.terminal,
        ctx.io_notify,
        ctx.renderer_wake,
    );
    thread.run();
    0
}

/// Stateful IO worker.
struct IoThread {
    console_session: Arc<ConPTY>,
    terminal: Arc<Terminal>,
    io_notify: Arc<IoThreadNotify>,
    renderer_wake: Arc<RendererWake>,

    resize_deadline: Option<Instant>,
    pending_resize: Option<TerminalDimensions>,
    sync_output_deadline: Option<Instant>,
}

impl IoThread {
    fn new(
        console_session: Arc<ConPTY>,
        terminal: Arc<Terminal>,
        io_notify: Arc<IoThreadNotify>,
        renderer_wake: Arc<RendererWake>,
    ) -> Self {
        Self {
            console_session,
            terminal,
            io_notify,
            renderer_wake,
            resize_deadline: None,
            pending_resize: None,
            sync_output_deadline: None,
        }
    }

    /// Queue-driven IO loop.
    ///
    /// Uses one alertable wait primitive to unify timer wakeups,
    /// queue wakeups (`NtAlertThread`), and write APC completions.
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
                continue;
            }

            self.fire_timers();

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
            IoMsg::Input(input) => match input {
                IoInput::Key(event) => self.console_session.send_key(event),
                IoInput::Mouse(event) => self.console_session.send_mouse(event),
                IoInput::Focus(focused) => self.console_session.send_focus(focused),
                IoInput::Paste(text) => self.console_session.send_paste(&text),
            },
            IoMsg::Resize(size) => {
                self.pending_resize = Some(size);
                self.resize_deadline =
                    Some(resize_deadline_after(self.resize_deadline, Instant::now()));
            }
            IoMsg::Scroll(op) => {
                self.apply_scroll(op);
            }
            IoMsg::StartSyncOutput => {
                self.sync_output_deadline = Some(Instant::now() + SYNC_OUTPUT_TIMEOUT);
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
                self.apply_resize(size);
            }
        }

        if let Some(deadline) = self.sync_output_deadline
            && now >= deadline
        {
            self.sync_output_deadline = None;
            self.terminal.reset_synchronized_output();
            self.renderer_wake.wake();
        }
    }

    /// Apply viewport scroll on the shared terminal.
    fn apply_scroll(&self, op: ScrollOp) {
        match op {
            ScrollOp::Delta(delta) => self.terminal.scroll_viewport(delta),
            ScrollOp::Top => self.terminal.scroll_to_top(),
            ScrollOp::Bottom => self.terminal.scroll_to_bottom(),
        }
        self.renderer_wake.wake();
    }

    fn apply_resize(&self, dimensions: TerminalDimensions) {
        let rows = (dimensions.screen_height_px / dimensions.cell_height_px) as u16;
        let cols = (dimensions.screen_width_px / dimensions.cell_width_px) as u16;

        self.terminal.resize(cols.max(1), rows.max(1));
        self.terminal.set_dimensions(dimensions);
        self.console_session.send_resize(cols.max(1), rows.max(1));
        self.renderer_wake.wake();
    }

    fn handle_close(&mut self) {
        self.pending_resize = None;
        self.resize_deadline = None;
        self.sync_output_deadline = None;
        while self.io_notify.queue.pop().is_some() {}
    }
}
