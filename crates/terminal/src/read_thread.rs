/// Read thread — the hot path.
///
/// Owns `conout`, the child process handle, and the raw HPCON (for resize).
/// Issues double-buffered overlapped reads and calls `terminal.feed()` inline,
/// eliminating the channel crossing + allocation that the v1 architecture paid
/// on every byte of PTY output.
///
/// Architecture mirrors Ghostty's `Exec.zig` / Windows Terminal's
/// `ConptyConnection` overlapped loop.
use std::io;
use std::os::windows::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use async_channel::Sender;

use bytes::Bytes;
use ghostty_vt::{Terminal, VtEvent};
use pty::{PtyReader, WindowSize};

use crate::types::{IoEvent, IoMsg, READ_NOTIFY_KEY, ReadThreadNotify};

// Windows APIs
use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_OPERATION_ABORTED, HANDLE, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::IO::{
    CancelIoEx, CreateIoCompletionPort, GetOverlappedResult, GetQueuedCompletionStatusEx,
    OVERLAPPED, OVERLAPPED_ENTRY, PostQueuedCompletionStatus,
};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, INFINITE};

const FILE_SKIP_COMPLETION_PORT_ON_SUCCESS: u8 = 0x1;

#[link(name = "Kernel32")]
unsafe extern "system" {
    fn SetFileCompletionNotificationModes(handle: HANDLE, flags: u8) -> i32;
}

// Two 64 KiB read buffers — heap-allocated, alternating roles.
// Windows Terminal uses the same size; large enough to amortize per-read
// overhead without wasting too much stack/heap on each session.
const READ_BUF_SIZE: usize = 64 * 1024;

/// Maximum number of IOCP completions to drain per poll.
const IOCP_BATCH_SIZE: usize = 128;
const IOCP_DRAIN_BATCH: usize = 8;

// Graceful shutdown timeout: if conout doesn't reach EOF within this
// duration after `closing` is set, force-terminate the child.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

/// Buffer state machine
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BufState {
    Idle,
    Pending,
}

struct ReadBuf {
    data: Box<[u8; READ_BUF_SIZE]>,
    overlapped: OVERLAPPED,
    state: BufState,
}

impl ReadBuf {
    fn new() -> Self {
        let overlapped = unsafe { std::mem::zeroed::<OVERLAPPED>() };
        Self {
            data: Box::new([0u8; READ_BUF_SIZE]),
            overlapped,
            state: BufState::Idle,
        }
    }
}

/// Outcome of issuing a `ReadFile` call.
///
/// `ReadFile` on an overlapped pipe can complete synchronously — the kernel
/// may return the data immediately but still post a completion to IOCP.
enum ReadStart {
    /// IO posted; completion will arrive via IOCP.
    Pending,
    /// Pipe closed / EOF.
    Eof,
    /// Unrecoverable IO error.
    Err(io::Error),
}

struct ReadThreadState {
    bufs: [ReadBuf; 2],
    /// `true` once the IO thread has signaled shutdown via `notify.closing`.
    draining: bool,
    /// Deadline for graceful drain-to-EOF; if expired, force-terminate child.
    shutdown_deadline: Option<Instant>,
    /// Sync-output state cached from the previous iteration.
    /// Used to detect the `false → true` edge (triggers `StartSyncOutput`).
    was_synchronized: bool,
    /// Exit status captured when EOF is observed.
    exit_status: Option<ExitStatus>,
    /// Reusable buffer for `drain_events()` — avoids per-read allocation.
    vt_event_buf: Vec<VtEvent>,
}

pub fn spawn(
    reader: PtyReader,
    terminal: Arc<Mutex<Terminal>>,
    notify: Arc<ReadThreadNotify>,
    io_tx: crossbeam_channel::Sender<IoMsg>,
    signal_tx: Sender<()>,
    event_tx: Sender<IoEvent>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("pty-read".into())
        .spawn(move || {
            read_loop(reader, terminal, notify, io_tx, signal_tx, event_tx);
        })
        .expect("failed to spawn read thread")
}

/// The Main Read loop
fn read_loop(
    reader: PtyReader,
    terminal: Arc<Mutex<Terminal>>,
    notify: Arc<ReadThreadNotify>,
    io_tx: crossbeam_channel::Sender<IoMsg>,
    signal_tx: Sender<()>,
    event_tx: Sender<IoEvent>,
) {
    let conout = reader.conout.raw();
    let child_handle = reader.child.handle();

    // Associate conout with the session IOCP.
    let assoc = unsafe { CreateIoCompletionPort(conout, notify.iocp, 0, 0) };
    if assoc.is_null() {
        let err = io::Error::last_os_error();
        log::error!("read_thread: CreateIoCompletionPort failed: {err}");
        let _ = event_tx.send_blocking(IoEvent::Error(err.to_string()));
        return;
    }
    // Ensure synchronous ReadFile completions do NOT post to IOCP; we manually
    // enqueue those so the main loop has a single completion path.
    let sfnm_ok =
        unsafe { SetFileCompletionNotificationModes(conout, FILE_SKIP_COMPLETION_PORT_ON_SUCCESS) }
            != 0;
    if !sfnm_ok {
        let err = io::Error::last_os_error();
        log::warn!("read_thread: SetFileCompletionNotificationModes failed: {err}");
    }

    let mut state = ReadThreadState {
        bufs: [ReadBuf::new(), ReadBuf::new()],
        draining: false,
        shutdown_deadline: None,
        was_synchronized: false,
        exit_status: None,
        vt_event_buf: Vec::new(),
    };

    // Issue initial reads on both buffers.
    // Invariant: we keep two reads in flight at all times (when not draining),
    // so IO and terminal processing stay overlapped.
    for buf in &mut state.bufs {
        match start_read(conout, notify.iocp, buf) {
            ReadStart::Pending => {}
            ReadStart::Eof => {
                emit_exit(&mut state, child_handle, &event_tx, &signal_tx);
                cleanup(&mut state, conout, notify.iocp);
                return;
            }
            ReadStart::Err(e) => {
                log::error!("read_thread: initial ReadFile failed: {e}");
                let _ = event_tx.send_blocking(IoEvent::Error(e.to_string()));
                cleanup(&mut state, conout, notify.iocp);
                return;
            }
        }
    }

    let mut entries: [OVERLAPPED_ENTRY; IOCP_BATCH_SIZE] = unsafe { std::mem::zeroed() };

    // ── Main IOCP loop ────────────────────────────────────────────────────────
    'main: loop {
        // Compute timeout for graceful-shutdown deadline.
        let timeout_ms: u32 = match state.shutdown_deadline {
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                // Clamp to u32::MAX ms (≈ 49 days) — practically never hit.
                remaining.as_millis().min(u32::MAX as u128) as u32
            }
            None => INFINITE,
        };

        let mut count: u32 = 0;
        let ok = unsafe {
            GetQueuedCompletionStatusEx(
                notify.iocp,
                entries.as_mut_ptr(),
                IOCP_BATCH_SIZE as u32,
                &mut count,
                timeout_ms,
                0,
            )
        };

        if ok == 0 {
            let err = io::Error::last_os_error();
            if err
                .raw_os_error()
                .map(|c| c as u32)
                .is_some_and(|c| c == WAIT_TIMEOUT)
            {
                if state.draining {
                    log::warn!("read_thread: shutdown deadline expired, force-terminating child");
                    reader.child.terminate();
                    state.shutdown_deadline = None; // stop re-firing the timeout
                }
                continue;
            }
            log::error!("read_thread: GetQueuedCompletionStatusEx failed: {err}");
            let _ = event_tx.send_blocking(IoEvent::Error(err.to_string()));
            break 'main;
        }

        for i in 0..count {
            let entry = entries[i as usize];
            if entry.lpCompletionKey == READ_NOTIFY_KEY {
                if notify.closing.load(Ordering::Acquire) && !state.draining {
                    state.draining = true;
                    state.shutdown_deadline = Some(Instant::now() + SHUTDOWN_TIMEOUT);
                }

                if let Some(size) = notify.take_resize() {
                    if !state.draining {
                        handle_resize(
                            size, conout, &mut state, &reader, &terminal, &notify, &io_tx,
                            &signal_tx, &event_tx,
                        );
                    }
                }
                continue;
            }

            let overlapped = entry.lpOverlapped;
            if overlapped.is_null() {
                continue;
            }

            let buf_index = if overlapped == &state.bufs[0].overlapped as *const _ as *mut _ {
                0
            } else if overlapped == &state.bufs[1].overlapped as *const _ as *mut _ {
                1
            } else {
                continue;
            };

            state.bufs[buf_index].state = BufState::Idle;
            match get_overlapped_result_iocp(
                conout,
                &state.bufs[buf_index].overlapped,
                entry.dwNumberOfBytesTransferred,
            ) {
                Ok(0) => {
                    emit_exit(&mut state, child_handle, &event_tx, &signal_tx);
                    break 'main;
                }
                Ok(n) => {
                    feed_and_dispatch(
                        &state.bufs[buf_index].data[..n as usize],
                        &mut state.was_synchronized,
                        state.draining,
                        &mut state.vt_event_buf,
                        &terminal,
                        &io_tx,
                        &signal_tx,
                        &event_tx,
                    );
                }
                Err(e) if is_broken_pipe(&e) => {
                    emit_exit(&mut state, child_handle, &event_tx, &signal_tx);
                    break 'main;
                }
                Err(e) => {
                    log::error!("read_thread: read completion failed: {e}");
                    let _ = event_tx.send_blocking(IoEvent::Error(e.to_string()));
                    break 'main;
                }
            }

            match start_read(conout, notify.iocp, &mut state.bufs[buf_index]) {
                ReadStart::Pending => {}
                ReadStart::Eof => {
                    emit_exit(&mut state, child_handle, &event_tx, &signal_tx);
                    break 'main;
                }
                ReadStart::Err(e) => {
                    log::error!("read_thread: ReadFile failed: {e}");
                    let _ = event_tx.send_blocking(IoEvent::Error(e.to_string()));
                    break 'main;
                }
            }
        }
    }

    cleanup(&mut state, conout, notify.iocp);
    // `reader` drops here — OwnedHandle(conout) and child handle are closed.
}

/// Feed `bytes` into the terminal, drain VT events, dispatch to channels,
/// and signal the renderer. Called from both the hot read path and the
/// resize-harvest path so that neither loses side effects.
fn feed_and_dispatch(
    bytes: &[u8],
    was_synchronized: &mut bool,
    draining: bool,
    vt_event_buf: &mut Vec<VtEvent>,
    terminal: &Arc<Mutex<Terminal>>,
    io_tx: &crossbeam_channel::Sender<IoMsg>,
    signal_tx: &Sender<()>,
    event_tx: &Sender<IoEvent>,
) {
    // ── Lock terminal, feed, drain ────────────────────────────────────────────
    let (sync_before, sync_after) = {
        let mut term = terminal.lock().expect("terminal mutex poisoned");
        let sync_before = *was_synchronized;
        term.feed(bytes);
        let sync_after = term.is_synchronized_output();
        term.drain_events(vt_event_buf);
        (sync_before, sync_after)
    };
    *was_synchronized = sync_after;

    // ── Dispatch VT events ────────────────────────────────────────────────────
    // drain(..) moves owned values out of the vec, avoiding extra clones while
    // preserving the backing allocation for reuse on the next call.
    for event in vt_event_buf.drain(..) {
        match event {
            VtEvent::DeviceResponse(bytes) => {
                // Blocking send: device responses are protocol-critical and must
                // not be dropped. Suppress only when draining (IO thread may be gone).
                if !draining {
                    io_tx.send(IoMsg::Reply(Bytes::from(bytes))).ok();
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

    // ── Sync-output edge detection ────────────────────────────────────────────
    // false → true edge: tell IO thread to arm the 1-second safety timer.
    // Blocking send: missing StartSyncOutput can permanently freeze the terminal.
    if !sync_before && sync_after && !draining {
        io_tx.send(IoMsg::StartSyncOutput).ok();
    }

    // ── Signal renderer ───────────────────────────────────────────────────────
    // Skip while synchronized (don't render partial frames).
    if !sync_after {
        signal_tx.try_send(()).ok();
    }
}

fn handle_resize(
    size: WindowSize,
    conout: HANDLE,
    state: &mut ReadThreadState,
    reader: &PtyReader,
    terminal: &Arc<Mutex<Terminal>>,
    notify: &Arc<ReadThreadNotify>,
    io_tx: &crossbeam_channel::Sender<IoMsg>,
    signal_tx: &Sender<()>,
    event_tx: &Sender<IoEvent>,
) {
    // Cancel all in-flight overlapped reads.
    for buf in &mut state.bufs {
        if buf.state == BufState::Pending {
            // ERROR_NOT_FOUND is expected if the IO already completed inline.
            unsafe { CancelIoEx(conout, &buf.overlapped as *const _ as *mut _) };
        }
    }

    // Non-blocking harvest: do NOT wait here. Blocking can deadlock if the IOCP
    // completion is still queued but not yet delivered.
    for i in 0..state.bufs.len() {
        if state.bufs[i].state != BufState::Pending {
            continue;
        }
        let mut bytes: u32 = 0;
        let ok = unsafe {
            GetOverlappedResult(
                conout,
                &state.bufs[i].overlapped as *const _ as *mut _,
                &mut bytes,
                0, // bWait = FALSE
            )
        };
        if ok != 0 {
            if bytes > 0 {
                // Data arrived before the cancel took effect — feed through the
                // full pipeline so Bell/Title/Reply/sync state are not lost.
                feed_and_dispatch(
                    &state.bufs[i].data[..bytes as usize],
                    &mut state.was_synchronized,
                    state.draining,
                    &mut state.vt_event_buf,
                    terminal,
                    io_tx,
                    signal_tx,
                    event_tx,
                );
            }
            // bytes == 0: EOF during harvest — ignore, the main loop will see it.
        } else {
            let e = io::Error::last_os_error();
            let code = e.raw_os_error().unwrap_or(0) as u32;
            if code != ERROR_OPERATION_ABORTED
                && code != windows_sys::Win32::Foundation::ERROR_IO_INCOMPLETE
                && !is_broken_pipe(&e)
            {
                log::warn!("read_thread: harvest after cancel: {e}");
            }
        }
        state.bufs[i].state = BufState::Idle;
    }

    // Drain queued completions before reusing the OVERLAPPED structs.
    drain_iocp_for_buffers(
        conout,
        notify.iocp,
        state,
        Some(notify),
        Some(terminal),
        Some(io_tx),
        Some(signal_tx),
        Some(event_tx),
    );

    // Resize ConPTY under hpcon_op to serialize with the IO thread's close path.
    {
        let _guard = notify.hpcon_op.lock().unwrap();
        if !notify.closing.load(Ordering::Acquire) {
            let coord = windows_sys::Win32::System::Console::COORD {
                X: size.num_cols as i16,
                Y: size.num_lines as i16,
            };
            // SAFETY: resize_fn and hpcon are valid for the lifetime of PtyReader.
            // hpcon_op serializes this against ClosePseudoConsole.
            let _ = unsafe { (reader.resize_fn)(reader.hpcon, coord) };
        }
    }

    // Update terminal grid dimensions.
    {
        let mut term = terminal.lock().expect("terminal mutex poisoned");
        term.set_cell_size(size.cell_width, size.cell_height);
        term.resize(size.num_cols, size.num_lines);
    }

    signal_tx.try_send(()).ok();

    // Re-arm reads on both buffers. Both bufs are Idle here (harvested above).
    for buf in &mut state.bufs {
        match start_read(conout, notify.iocp, buf) {
            ReadStart::Pending => {}
            ReadStart::Eof => {
                // EOF will be handled by the main loop once completion arrives.
            }
            ReadStart::Err(e) => {
                log::error!("read_thread: handle_resize ReadFile failed: {e}");
                let _ = event_tx.send_blocking(IoEvent::Error(e.to_string()));
            }
        }
    }
}

fn cleanup(state: &mut ReadThreadState, conout: HANDLE, iocp: HANDLE) {
    // Cancel any still-pending reads and drain queued completions. We must not drop
    // the OVERLAPPED structs while IO is in flight.
    for buf in &mut state.bufs {
        if buf.state == BufState::Pending {
            unsafe { CancelIoEx(conout, &buf.overlapped as *const _ as *mut _) };
            let mut bytes: u32 = 0;
            unsafe {
                GetOverlappedResult(
                    conout,
                    &buf.overlapped as *const _ as *mut _,
                    &mut bytes,
                    0, // bWait = FALSE
                )
            };
            buf.state = BufState::Idle;
        }
    }
    drain_iocp_for_buffers(conout, iocp, state, None, None, None, None, None);
}

fn drain_iocp_for_buffers(
    conout: HANDLE,
    iocp: HANDLE,
    state: &mut ReadThreadState,
    notify: Option<&ReadThreadNotify>,
    terminal: Option<&Arc<Mutex<Terminal>>>,
    io_tx: Option<&crossbeam_channel::Sender<IoMsg>>,
    signal_tx: Option<&Sender<()>>,
    event_tx: Option<&Sender<IoEvent>>,
) {
    let mut entries: [OVERLAPPED_ENTRY; IOCP_DRAIN_BATCH] = unsafe { std::mem::zeroed() };
    let track_notify = notify.is_some();
    let mut saw_notify = false;
    loop {
        let mut count: u32 = 0;
        let ok = unsafe {
            GetQueuedCompletionStatusEx(
                iocp,
                entries.as_mut_ptr(),
                entries.len() as u32,
                &mut count,
                0, // timeout = 0 (poll)
                0,
            )
        };
        if ok == 0 || count == 0 {
            break;
        }
        for i in 0..count {
            let entry = entries[i as usize];
            if entry.lpCompletionKey == READ_NOTIFY_KEY {
                if track_notify {
                    saw_notify = true;
                }
                continue;
            }
            let overlapped = entry.lpOverlapped;
            if overlapped.is_null() {
                continue;
            }
            let buf_index = if overlapped == &state.bufs[0].overlapped as *const _ as *mut _ {
                0
            } else if overlapped == &state.bufs[1].overlapped as *const _ as *mut _ {
                1
            } else {
                continue;
            };

            state.bufs[buf_index].state = BufState::Idle;
            let bytes = match get_overlapped_result_iocp(
                conout,
                &state.bufs[buf_index].overlapped,
                entry.dwNumberOfBytesTransferred,
            ) {
                Ok(n) => n,
                Err(_) => 0,
            };
            if bytes == 0 {
                continue;
            }
            if let (Some(terminal), Some(io_tx), Some(signal_tx), Some(event_tx)) =
                (terminal, io_tx, signal_tx, event_tx)
            {
                feed_and_dispatch(
                    &state.bufs[buf_index].data[..bytes as usize],
                    &mut state.was_synchronized,
                    state.draining,
                    &mut state.vt_event_buf,
                    terminal,
                    io_tx,
                    signal_tx,
                    event_tx,
                );
            }
        }
    }
    if saw_notify {
        if let Some(notify) = notify {
            notify.signal();
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Issue a `ReadFile` on an overlapped pipe handle.
///
/// Returns `Pending` when ERROR_IO_PENDING, `Ready(n)` for synchronous
/// completions, `Eof` for 0 bytes / broken pipe, and `Err` for other errors.
///
/// On `Pending`, sets `buf.state = BufState::Pending`.
fn start_read(handle: HANDLE, iocp: HANDLE, buf: &mut ReadBuf) -> ReadStart {
    debug_assert_eq!(buf.state, BufState::Idle);

    // Zero-out offset fields — named pipes don't use them, but zeroing
    // prevents OVERLAPPED reuse from confusing the kernel.
    buf.overlapped.Anonymous.Anonymous.Offset = 0;
    buf.overlapped.Anonymous.Anonymous.OffsetHigh = 0;
    // Reset internal fields so IOCP sees a clean OVERLAPPED.
    buf.overlapped.Internal = 0;
    buf.overlapped.InternalHigh = 0;

    let mut bytes_read: u32 = 0;
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::ReadFile(
            handle,
            buf.data.as_mut_ptr(),
            READ_BUF_SIZE as u32,
            &mut bytes_read,
            &mut buf.overlapped,
        )
    };

    if ok != 0 {
        if bytes_read == 0 {
            return ReadStart::Eof;
        }
        // Synchronous completion. We manually enqueue it to the IOCP so the
        // main loop sees a single completion path.
        let queued =
            unsafe { PostQueuedCompletionStatus(iocp, bytes_read, 0, &mut buf.overlapped) };
        if queued == 0 {
            return ReadStart::Err(io::Error::last_os_error());
        }
        buf.state = BufState::Pending;
        return ReadStart::Pending;
    }

    let err = io::Error::last_os_error();
    let code = err.raw_os_error().unwrap_or(0) as u32;

    if code == windows_sys::Win32::Foundation::ERROR_IO_PENDING {
        buf.state = BufState::Pending;
        return ReadStart::Pending;
    }
    if is_broken_pipe(&err) {
        return ReadStart::Eof;
    }
    ReadStart::Err(err)
}

/// Translate an IOCP completion into a byte count or error.
fn get_overlapped_result_iocp(
    handle: HANDLE,
    overlapped: &OVERLAPPED,
    bytes_transferred: u32,
) -> io::Result<u32> {
    if bytes_transferred > 0 {
        return Ok(bytes_transferred);
    }
    let mut bytes: u32 = 0;
    let ok = unsafe {
        GetOverlappedResult(
            handle,
            overlapped as *const _ as *mut _,
            &mut bytes,
            0, // bWait = FALSE
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(bytes)
}

fn capture_exit_status(child_handle: HANDLE) -> Option<ExitStatus> {
    let mut exit_code: u32 = 0;
    let ok = unsafe { GetExitCodeProcess(child_handle, &mut exit_code) };
    if ok == 0 || exit_code == 259 {
        return None;
    }
    Some(ExitStatus::from_raw(exit_code))
}

fn emit_exit(
    state: &mut ReadThreadState,
    child_handle: HANDLE,
    event_tx: &Sender<IoEvent>,
    signal_tx: &Sender<()>,
) {
    if state.exit_status.is_none() {
        state.exit_status = capture_exit_status(child_handle);
    }
    emit_exited(state.exit_status, event_tx, signal_tx);
}

/// Emit a process-exit event to both channels, then wake the renderer.
fn emit_exited(
    exit_status: Option<ExitStatus>,
    event_tx: &Sender<IoEvent>,
    signal_tx: &Sender<()>,
) {
    // send_blocking: Exited must not be lost even if the channel is momentarily full.
    let _ = event_tx.send_blocking(IoEvent::Exited(exit_status));
    signal_tx.try_send(()).ok();
}

/// Returns `true` when the error represents a closed/EOF pipe.
fn is_broken_pipe(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error().map(|c| c as u32),
        Some(ERROR_BROKEN_PIPE) | Some(232) // ERROR_NO_DATA (pipe closing)
    )
}
