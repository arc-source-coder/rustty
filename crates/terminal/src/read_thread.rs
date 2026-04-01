/// Read thread — hot output path.
///
/// Owns `conout`, child process handle, and raw HPCON resize access.
/// Uses `NtReadFile` + APC completion with a 4-buffer pipeline.
///
/// Main loop shape:
/// 1) arm idle read buffers,
/// 2) alertable wait (`NtDelayExecution`),
/// 3) harvest completed buffers,
/// 4) process resize/close signals from IO thread.
use std::io;
use std::os::windows::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_channel::Sender;
use bytes::Bytes;
use ghostty::{Terminal, VtEvent};
use pty::{PtyReader, WindowSize};
use windows_sys::Win32::Foundation::HANDLE;

use crate::platform::windows::io::{AsyncIo, alertable_wait, async_read, cancel_io};
use crate::platform::windows::ntdll::{
    NtQueryInformationProcess, PROCESS_INFORMATION_CLASS_BASIC_INFORMATION,
    ProcessBasicInformation, STATUS_ALERTED, STATUS_CANCELLED, STATUS_END_OF_FILE, STATUS_PENDING,
    STATUS_PIPE_BROKEN, STATUS_SUCCESS, STATUS_USER_APC,
};
use crate::platform::windows::thread::{PlatformThread, set_current_thread_name};
use crate::types::{IoEvent, IoMsg, IoThreadNotify, ReadThreadNotify};

const NUM_READ_BUFS: usize = 4;
const READ_BUF_SIZE: usize = 64 * 1024;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

/// State for one read buffer in the APC pipeline.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BufState {
    Idle,
    InFlight,
}

struct ReadBuf {
    data: Box<[u8; READ_BUF_SIZE]>,
    io: AsyncIo,
    state: BufState,
    seq: u64,
}

impl ReadBuf {
    fn new() -> Self {
        Self {
            data: Box::new([0u8; READ_BUF_SIZE]),
            io: AsyncIo::new(),
            state: BufState::Idle,
            seq: 0,
        }
    }
}

struct ReadThreadState {
    bufs: [ReadBuf; NUM_READ_BUFS],
    draining: bool,
    shutdown_deadline: Option<Instant>,
    was_synchronized: bool,
    exit_status: Option<ExitStatus>,
    vt_event_buf: Vec<VtEvent>,
    next_issue_seq: u64,
    next_harvest_seq: u64,
    saw_eof: bool,
    forced_terminate: bool,
}

/// Context passed through `NtCreateThreadEx` start routine.
struct ReadThreadContext {
    reader: PtyReader,
    terminal: Arc<Mutex<Terminal>>,
    notify: Arc<ReadThreadNotify>,
    io_notify: Arc<IoThreadNotify>,
    signal_tx: Sender<()>,
    event_tx: Sender<IoEvent>,
}

pub fn spawn_suspended(
    reader: PtyReader,
    terminal: Arc<Mutex<Terminal>>,
    notify: Arc<ReadThreadNotify>,
    io_notify: Arc<IoThreadNotify>,
    signal_tx: Sender<()>,
    event_tx: Sender<IoEvent>,
) -> io::Result<PlatformThread> {
    let ctx = Box::new(ReadThreadContext {
        reader,
        terminal,
        notify,
        io_notify,
        signal_tx,
        event_tx,
    });
    let ctx_ptr = Box::into_raw(ctx) as *mut std::ffi::c_void;
    match PlatformThread::spawn_suspended(read_thread_entry, ctx_ptr) {
        Ok(thread) => Ok(thread),
        Err(err) => {
            // SAFETY: ctx_ptr was produced by Box::into_raw above.
            unsafe {
                drop(Box::from_raw(ctx_ptr as *mut ReadThreadContext));
            }
            Err(err)
        }
    }
}

/// Ntdll thread entry trampoline.
unsafe extern "system" fn read_thread_entry(context: *mut std::ffi::c_void) -> u32 {
    set_current_thread_name("pty-read");
    #[cfg(feature = "profiler")]
    tracy_client::set_thread_name!("pty-read");
    // SAFETY: context comes from Box::into_raw in spawn_suspended.
    let ctx = unsafe { Box::from_raw(context as *mut ReadThreadContext) };
    read_loop(
        ctx.reader,
        ctx.terminal,
        ctx.notify,
        ctx.io_notify,
        ctx.signal_tx,
        ctx.event_tx,
    );
    0
}

/// Main read loop.
///
/// Keeps multiple reads in flight and dispatches VT side effects inline to
/// avoid extra allocations and cross-thread copies on terminal output.
fn read_loop(
    reader: PtyReader,
    terminal: Arc<Mutex<Terminal>>,
    notify: Arc<ReadThreadNotify>,
    io_notify: Arc<IoThreadNotify>,
    signal_tx: Sender<()>,
    event_tx: Sender<IoEvent>,
) {
    let conout = reader.conout.raw();
    let child_handle = reader.child.handle();

    let mut state = ReadThreadState {
        bufs: std::array::from_fn(|_| ReadBuf::new()),
        draining: false,
        shutdown_deadline: None,
        was_synchronized: false,
        exit_status: None,
        vt_event_buf: Vec::new(),
        next_issue_seq: 0,
        next_harvest_seq: 0,
        saw_eof: false,
        forced_terminate: false,
    };

    loop {
        if !state.saw_eof {
            match arm_idle_reads(conout, &mut state, &event_tx) {
                ArmResult::Continue => {}
                ArmResult::Eof => state.saw_eof = true,
                ArmResult::Error => break,
            }
        }

        // Fast path: if any completion APC already ran while arming reads,
        // harvest before entering the next alertable wait.
        if handle_harvest_result(
            harvest_done_buffers(&mut state, &terminal, &io_notify, &signal_tx, &event_tx),
            &mut state,
            child_handle,
            &event_tx,
            &signal_tx,
        ) {
            break;
        }

        if state.saw_eof && count_inflight(&state) == 0 {
            emit_exit(&mut state, child_handle, &event_tx, &signal_tx);
            break;
        }

        if let Some(deadline) = state.shutdown_deadline
            && Instant::now() >= deadline
        {
            if !state.forced_terminate {
                log::warn!("read_thread: shutdown deadline expired, force-terminating child");
                reader.child.terminate();
                state.forced_terminate = true;
            }
            state.shutdown_deadline = None;
        }

        let wake_status = alertable_wait(timeout_100ns(state.shutdown_deadline));
        if wake_status != STATUS_SUCCESS
            && wake_status != STATUS_ALERTED
            && wake_status != STATUS_USER_APC
        {
            log::warn!("read_thread: unexpected NtDelayExecution status=0x{wake_status:08X}");
        }

        if handle_harvest_result(
            harvest_done_buffers(&mut state, &terminal, &io_notify, &signal_tx, &event_tx),
            &mut state,
            child_handle,
            &event_tx,
            &signal_tx,
        ) {
            break;
        }

        if notify.closing.load(std::sync::atomic::Ordering::Acquire) && !state.draining {
            state.draining = true;
            state.shutdown_deadline = Some(Instant::now() + SHUTDOWN_TIMEOUT);
        }

        if let Some(size) = notify.take_resize()
            && !state.draining
            && handle_harvest_result(
                handle_resize(
                    size, conout, &reader, &mut state, &terminal, &notify, &io_notify, &signal_tx,
                    &event_tx,
                ),
                &mut state,
                child_handle,
                &event_tx,
                &signal_tx,
            )
        {
            break;
        }
    }

    cleanup(&mut state, conout);
}

// ─── Arm / Harvest ───────────────────────────────────────────────────────────

/// Start reads on all idle buffers.
///
/// For stream correctness we attach a monotonic sequence number to each
/// issued buffer and harvest completions strictly in sequence order.
enum ArmResult {
    Continue,
    Eof,
    Error,
}

fn arm_idle_reads(
    conout: HANDLE,
    state: &mut ReadThreadState,
    event_tx: &Sender<IoEvent>,
) -> ArmResult {
    for buf in state.bufs.iter_mut() {
        if buf.state != BufState::Idle {
            continue;
        }
        let status = unsafe {
            async_read(
                conout,
                &mut buf.io,
                buf.data.as_mut_ptr(),
                READ_BUF_SIZE as u32,
            )
        };
        match status {
            STATUS_SUCCESS | STATUS_PENDING => {
                buf.state = BufState::InFlight;
                buf.seq = state.next_issue_seq;
                state.next_issue_seq = state.next_issue_seq.wrapping_add(1);
            }
            STATUS_END_OF_FILE | STATUS_PIPE_BROKEN => return ArmResult::Eof,
            _ => {
                log::error!("read_thread: NtReadFile failed: 0x{status:08X}");
                let _ = event_tx
                    .send_blocking(IoEvent::Error(format!("NtReadFile failed: 0x{status:08X}")));
                return ArmResult::Error;
            }
        }
    }
    ArmResult::Continue
}

enum Harvest {
    Continue,
    Eof,
    Error,
}

enum DrainMode<'a> {
    Dispatch {
        terminal: &'a Arc<Mutex<Terminal>>,
        io_notify: &'a Arc<IoThreadNotify>,
        signal_tx: &'a Sender<()>,
        event_tx: &'a Sender<IoEvent>,
    },
    Silent,
}

/// Consume completed buffers in stream order.
///
/// Finds the buffer with `seq == next_harvest_seq` and checks its `done`
/// flag (set by the APC callback). With 4 buffers the scan is trivial.
#[allow(clippy::too_many_arguments)]
fn harvest_done_buffers(
    state: &mut ReadThreadState,
    terminal: &Arc<Mutex<Terminal>>,
    io_notify: &Arc<IoThreadNotify>,
    signal_tx: &Sender<()>,
    event_tx: &Sender<IoEvent>,
) -> Harvest {
    harvest_buffers(state, terminal, io_notify, signal_tx, event_tx, |buf| {
        buf.io.done
    })
}

/// Consume completed buffers in stream order for cancellation-drain paths.
///
/// During cancellation we accept either APC completion (`io.done`) or
/// IOSB transition away from `STATUS_PENDING` to avoid deadlock when APC
/// delivery races with cancellation.
#[allow(clippy::too_many_arguments)]
fn harvest_cancel_visible_buffers(
    state: &mut ReadThreadState,
    terminal: &Arc<Mutex<Terminal>>,
    io_notify: &Arc<IoThreadNotify>,
    signal_tx: &Sender<()>,
    event_tx: &Sender<IoEvent>,
) -> Harvest {
    harvest_buffers(
        state,
        terminal,
        io_notify,
        signal_tx,
        event_tx,
        is_completion_visible,
    )
}

#[allow(clippy::too_many_arguments)]
fn harvest_buffers(
    state: &mut ReadThreadState,
    terminal: &Arc<Mutex<Terminal>>,
    io_notify: &Arc<IoThreadNotify>,
    signal_tx: &Sender<()>,
    event_tx: &Sender<IoEvent>,
    is_ready: fn(&ReadBuf) -> bool,
) -> Harvest {
    loop {
        let idx = state
            .bufs
            .iter()
            .position(|buf| buf.state == BufState::InFlight && buf.seq == state.next_harvest_seq);
        let Some(idx) = idx else { break };

        if !is_ready(&state.bufs[idx]) {
            break;
        }

        let status = state.bufs[idx].io.iosb.status();
        let bytes = state.bufs[idx].io.iosb.information;

        match status {
            STATUS_SUCCESS if bytes > 0 => {
                if bytes > READ_BUF_SIZE {
                    log::error!("read_thread: invalid byte count from IOSB: {bytes}");
                    let _ = event_tx.send_blocking(IoEvent::Error(format!(
                        "invalid NtReadFile byte count: {bytes}"
                    )));
                    return Harvest::Error;
                }
                feed_and_dispatch(
                    &state.bufs[idx].data[..bytes],
                    &mut state.was_synchronized,
                    state.draining,
                    &mut state.vt_event_buf,
                    terminal,
                    io_notify,
                    signal_tx,
                    event_tx,
                );
            }
            STATUS_SUCCESS => {}
            STATUS_END_OF_FILE | STATUS_PIPE_BROKEN => return Harvest::Eof,
            STATUS_CANCELLED => {}
            _ => {
                log::error!("read_thread: read completion failed: 0x{status:08X}");
                let _ = event_tx.send_blocking(IoEvent::Error(format!(
                    "NtReadFile completion failed: 0x{status:08X}"
                )));
                return Harvest::Error;
            }
        }

        state.bufs[idx].state = BufState::Idle;
        state.bufs[idx].seq = 0;
        state.next_harvest_seq = state.next_harvest_seq.wrapping_add(1);
    }

    Harvest::Continue
}

// ─── Resize / Cleanup ────────────────────────────────────────────────────────

/// Resize path:
/// Cancel in-flight reads, drain completions, resize ConPTY + terminal grid.
#[allow(clippy::too_many_arguments)]
fn handle_resize(
    size: WindowSize,
    conout: HANDLE,
    reader: &PtyReader,
    state: &mut ReadThreadState,
    terminal: &Arc<Mutex<Terminal>>,
    notify: &Arc<ReadThreadNotify>,
    io_notify: &Arc<IoThreadNotify>,
    signal_tx: &Sender<()>,
    event_tx: &Sender<IoEvent>,
) -> Harvest {
    cancel_inflight(state, conout);
    match drain_cancelled(
        state,
        DrainMode::Dispatch {
            terminal,
            io_notify,
            signal_tx,
            event_tx,
        },
    ) {
        Harvest::Continue => {}
        other => return other,
    }

    {
        let _guard = notify.hpcon_op.lock().unwrap();
        if !notify.closing.load(std::sync::atomic::Ordering::Acquire) {
            let coord = windows_sys::Win32::System::Console::COORD {
                X: size.num_cols as i16,
                Y: size.num_lines as i16,
            };
            let _ = unsafe { (reader.resize_fn)(reader.hpcon, coord) };
        }
    }

    {
        #[cfg(feature = "profiler")]
        let _c = tracy_client::span!("handle_resize:contention", 32);
        let mut term = terminal.lock().expect("terminal mutex poisoned");
        #[cfg(feature = "profiler")]
        let _h = tracy_client::span!("handle_resize:hold", 32);
        term.set_cell_size(size.cell_width, size.cell_height);
        term.resize(size.num_cols, size.num_lines);
        term.reset_synchronized_output();
    }
    signal_tx.try_send(()).ok();
    Harvest::Continue
}

/// Cancel all in-flight reads.
fn cancel_inflight(state: &mut ReadThreadState, conout: HANDLE) {
    for buf in &mut state.bufs {
        if buf.state == BufState::InFlight {
            let _ = unsafe { cancel_io(conout, &buf.io.iosb) };
        }
    }
}

/// Cancel all in-flight reads and wait for their APC completions.
fn cleanup(state: &mut ReadThreadState, conout: HANDLE) {
    cancel_inflight(state, conout);
    let _ = drain_cancelled(state, DrainMode::Silent);
}

/// Drain cancelled in-flight reads, feeding any data that arrived before
/// cancellation took effect through the normal harvest pipeline.
fn drain_cancelled(state: &mut ReadThreadState, mode: DrainMode<'_>) -> Harvest {
    loop {
        let before = count_inflight(state);
        if before == 0 {
            return Harvest::Continue;
        }

        let result = match &mode {
            DrainMode::Dispatch {
                terminal,
                io_notify,
                signal_tx,
                event_tx,
            } => harvest_cancel_visible_buffers(state, terminal, io_notify, signal_tx, event_tx),
            DrainMode::Silent => {
                retire_cancel_visible_inflight(state);
                Harvest::Continue
            }
        };

        match result {
            Harvest::Continue => {}
            other => return other,
        }

        let after = count_inflight(state);
        if after == 0 {
            return Harvest::Continue;
        }

        if after == before {
            let _ = alertable_wait(-100_000); // 10ms
        }
    }
}

/// Retire cancellation-visible in-flight buffers without terminal dispatch.
fn retire_cancel_visible_inflight(state: &mut ReadThreadState) {
    for buf in &mut state.bufs {
        if buf.state == BufState::InFlight && is_completion_visible(buf) {
            buf.state = BufState::Idle;
            buf.seq = 0;
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn count_inflight(state: &ReadThreadState) -> usize {
    state
        .bufs
        .iter()
        .filter(|buf| buf.state == BufState::InFlight)
        .count()
}

/// A buffer's completion is visible if the APC callback set `done`,
/// or if the IOSB has transitioned away from `STATUS_PENDING` (which
/// can happen during cancellation when the APC delivery is lost).
fn is_completion_visible(buf: &ReadBuf) -> bool {
    buf.io.done || buf.io.iosb.status() != STATUS_PENDING
}

fn handle_harvest_result(
    result: Harvest,
    state: &mut ReadThreadState,
    child_handle: HANDLE,
    event_tx: &Sender<IoEvent>,
    signal_tx: &Sender<()>,
) -> bool {
    match result {
        Harvest::Continue => false,
        Harvest::Eof => {
            emit_exit(state, child_handle, event_tx, signal_tx);
            true
        }
        Harvest::Error => true,
    }
}

/// Feed bytes into terminal, dispatch VT side effects, and wake renderer.
#[allow(clippy::too_many_arguments)]
fn feed_and_dispatch(
    bytes: &[u8],
    was_synchronized: &mut bool,
    draining: bool,
    vt_event_buf: &mut Vec<VtEvent>,
    terminal: &Arc<Mutex<Terminal>>,
    io_notify: &Arc<IoThreadNotify>,
    signal_tx: &Sender<()>,
    event_tx: &Sender<IoEvent>,
) {
    let (sync_before, sync_after) = {
        #[cfg(feature = "profiler")]
        let _c = tracy_client::span!("feed_and_dispatch:contention", 32);
        let mut term = terminal.lock().expect("terminal mutex poisoned");
        #[cfg(feature = "profiler")]
        let _h = tracy_client::span!("feed_and_dispatch:hold", 32);
        let sync_before = *was_synchronized;
        term.feed(bytes);
        term.drain_events(vt_event_buf);
        let sync_after = term.is_synchronized_output();
        (sync_before, sync_after)
    };

    *was_synchronized = sync_after;

    for event in vt_event_buf.drain(..) {
        match event {
            VtEvent::DeviceResponse(bytes) => {
                if !draining {
                    io_notify.send_lossless(IoMsg::Reply(Bytes::from(bytes)));
                }
            }
            VtEvent::Bell => {
                event_tx.try_send(IoEvent::Bell).ok();
            }
            VtEvent::TitleChanged(title) => {
                event_tx.try_send(IoEvent::TitleChanged(title)).ok();
            }
        }
    }

    if !sync_before && sync_after && !draining {
        io_notify.send_lossless(IoMsg::StartSyncOutput);
    }

    if !sync_after {
        signal_tx.try_send(()).ok();
    }
}

/// Read process exit code from child process handle.
fn capture_exit_status(child_handle: HANDLE) -> Option<ExitStatus> {
    let mut pbi = ProcessBasicInformation {
        exit_status: STATUS_PENDING,
        peb_base_address: std::ptr::null_mut(),
        affinity_mask: 0,
        base_priority: 0,
        unique_process_id: 0,
        inherited_from_unique_process_id: 0,
    };
    let status = unsafe {
        NtQueryInformationProcess(
            child_handle,
            PROCESS_INFORMATION_CLASS_BASIC_INFORMATION,
            &mut pbi as *mut _ as *mut std::ffi::c_void,
            std::mem::size_of::<ProcessBasicInformation>() as u32,
            std::ptr::null_mut(),
        )
    };
    if status != STATUS_SUCCESS || pbi.exit_status == STATUS_PENDING {
        return None;
    }
    Some(ExitStatus::from_raw(pbi.exit_status as u32))
}

/// Emit `IoEvent::Exited` once.
fn emit_exit(
    state: &mut ReadThreadState,
    child_handle: HANDLE,
    event_tx: &Sender<IoEvent>,
    signal_tx: &Sender<()>,
) {
    if state.exit_status.is_none() {
        state.exit_status = capture_exit_status(child_handle);
    }
    let _ = event_tx.send_blocking(IoEvent::Exited(state.exit_status));
    signal_tx.try_send(()).ok();
}

/// Convert an absolute deadline to a relative NT interval (100ns units, 64-bit).
fn timeout_100ns(deadline: Option<Instant>) -> i64 {
    match deadline {
        None => i64::MIN,
        Some(deadline) => {
            let d = deadline.saturating_duration_since(Instant::now());
            let ticks = d
                .as_secs()
                .saturating_mul(10_000_000)
                .saturating_add((d.subsec_nanos() / 100) as u64)
                .min(i64::MAX as u64) as i64;
            if ticks == 0 { 0 } else { -ticks }
        }
    }
}
