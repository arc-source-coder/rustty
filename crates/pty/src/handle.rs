use async_channel::{Receiver, Sender};
use std::collections::VecDeque;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::{
    BufferPool, Options, OutputBuffer, PTY_COMMAND_CHANNEL_CAPACITY, PTY_EVENT_CHANNEL_CAPACITY,
    Pty, PtyCommand, PtyEvent, WindowSize,
};

const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

pub struct PtyHandle {
    pub event_rx: Receiver<PtyEvent>,
    pub command_tx: Sender<PtyCommand>,
    worker: Option<JoinHandle<()>>,
}

impl PtyHandle {
    pub fn spawn(options: Options, window_size: WindowSize) -> std::io::Result<Self> {
        let (event_tx, event_rx) = async_channel::bounded(PTY_EVENT_CHANNEL_CAPACITY);
        let (command_tx, command_rx) = async_channel::bounded(PTY_COMMAND_CHANNEL_CAPACITY);

        // Spawn the platform PTY before moving to worker thread
        // so we can return spawn errors synchronously.
        let pty = crate::new(&options, window_size)?;
        let command_signal_tx = command_tx.clone();

        let worker = thread::Builder::new()
            .name("pty-worker".into())
            .spawn(move || {
                worker_loop(pty, event_tx, command_rx, command_signal_tx);
            })
            .map_err(|e| std::io::Error::other(format!("failed to spawn pty worker: {e}")))?;

        Ok(Self {
            event_rx,
            command_tx,
            worker: Some(worker),
        })
    }
}

impl Drop for PtyHandle {
    fn drop(&mut self) {
        // Unblock worker sends first: if the worker is stuck in
        // send_blocking(Output) with no active consumer, closing the receiver
        // makes that send fail immediately so shutdown can proceed.
        self.event_rx.close();
        let _ = self.command_tx.try_send(PtyCommand::Close);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn worker_loop(
    pty: Pty,
    event_tx: Sender<PtyEvent>,
    command_rx: Receiver<PtyCommand>,
    command_signal_tx: Sender<PtyCommand>,
) {
    windows_worker_loop(pty, event_tx, command_rx, command_signal_tx);
}

fn create_event(manual_reset: bool) -> std::io::Result<windows_sys::Win32::Foundation::HANDLE> {
    let handle = unsafe {
        windows_sys::Win32::System::Threading::CreateEventW(
            std::ptr::null_mut(),
            manual_reset as i32,
            0,
            std::ptr::null(),
        )
    };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    Ok(handle)
}

fn windows_worker_loop(
    mut pty: Pty,
    event_tx: Sender<PtyEvent>,
    command_rx: Receiver<PtyCommand>,
    command_signal_tx: Sender<PtyCommand>,
) {
    use std::sync::{Arc, Mutex};

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Threading::{
        GetCurrentThreadId, INFINITE, SetEvent, WaitForMultipleObjects,
    };

    const MAX_WRITE_CHUNK: usize = 64 * 1024;

    let cmd_event = match create_event(false) {
        Ok(h) => h,
        Err(e) => {
            event_tx.send_blocking(PtyEvent::Error(e)).ok();
            return;
        }
    };
    let read_event = match create_event(false) {
        Ok(h) => h,
        Err(e) => {
            unsafe { CloseHandle(cmd_event) };
            event_tx.send_blocking(PtyEvent::Error(e)).ok();
            return;
        }
    };
    let write_event = match create_event(false) {
        Ok(h) => h,
        Err(e) => {
            unsafe {
                CloseHandle(read_event);
                CloseHandle(cmd_event)
            };
            event_tx.send_blocking(PtyEvent::Error(e)).ok();
            return;
        }
    };

    // async_channel::Receiver can't be waited on via WaitForMultipleObjects.
    // Bridge it into a waitable event + shared queue.
    let queued_commands = Arc::new(Mutex::new(VecDeque::<PtyCommand>::new()));
    let queued_commands_tx = queued_commands.clone();
    let cmd_thread_event = cmd_event as usize;
    let cmd_forwarder = thread::Builder::new()
        .name(format!("pty-cmd-forward-{}", unsafe {
            GetCurrentThreadId()
        }))
        .spawn(move || {
            while let Ok(cmd) = command_rx.recv_blocking() {
                let is_close = matches!(cmd, PtyCommand::Close);
                queued_commands_tx.lock().unwrap().push_back(cmd);
                unsafe {
                    SetEvent(cmd_thread_event as HANDLE);
                }
                if is_close {
                    return;
                }
            }

            queued_commands_tx
                .lock()
                .unwrap()
                .push_back(PtyCommand::Close);
            unsafe {
                SetEvent(cmd_thread_event as HANDLE);
            }
        });

    let mut read_overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    read_overlapped.hEvent = read_event;
    let mut write_overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    write_overlapped.hEvent = write_event;

    let pool = BufferPool::new(128 * 1024, 4);
    let mut read_buf = pool.acquire();
    let mut write_buf = VecDeque::<u8>::new();
    let mut write_inflight = Vec::<u8>::new();
    let mut read_pending;
    let mut write_pending = false;
    let mut closing = false;
    let mut child_exited = false;
    let mut shutdown_deadline: Option<Instant> = None;

    let conout = pty.conout_handle();
    let conin = pty.conin_handle();
    let child = pty.child_handle();

    // Queue first read.
    let (pending, output) = queue_read(
        conout,
        &mut read_overlapped,
        &mut read_buf,
        &pool,
        &event_tx,
        &mut closing,
        &mut shutdown_deadline,
    );
    read_pending = pending;
    if let Some(output) = output
        && !emit_output(&event_tx, false, output)
    {
        return;
    }

    while !child_exited {
        if !write_pending && (!write_inflight.is_empty() || !write_buf.is_empty()) {
            if write_inflight.is_empty() {
                let write_len = write_buf.len().min(MAX_WRITE_CHUNK);
                write_inflight.extend(write_buf.drain(..write_len));
            }
            match start_write(conin, &mut write_overlapped, &write_inflight) {
                Ok(OverlappedStart::Pending) => {
                    write_pending = true;
                }
                Ok(OverlappedStart::Completed(written)) => {
                    write_pending = false;
                    if written >= write_inflight.len() {
                        write_inflight.clear();
                    } else {
                        write_inflight.drain(..written);
                    }
                }
                Err(e) => {
                    report_non_broken_pipe_error(&event_tx, e);
                    mark_closing(&mut closing, &mut shutdown_deadline);
                    write_pending = false;
                    write_inflight.clear();
                }
            }
        }

        let mut handles: [HANDLE; 4] = [cmd_event, child, read_event, write_event];
        let mut handle_count = 3;
        if write_pending {
            handle_count = 4;
        }

        let timeout = match shutdown_deadline {
            Some(deadline) => deadline
                .checked_duration_since(Instant::now())
                .map_or(0, |remaining| {
                    remaining.as_millis().min(u32::MAX as u128) as u32
                }),
            None => INFINITE,
        };

        let wait_result =
            unsafe { WaitForMultipleObjects(handle_count, handles.as_mut_ptr(), 0, timeout) };
        match wait_result {
            x if x == WAIT_OBJECT_0 => {
                let mut pending_resize = None;
                let mut drained = VecDeque::new();
                {
                    let mut queue = queued_commands.lock().unwrap();
                    std::mem::swap(&mut *queue, &mut drained);
                }

                for cmd in drained {
                    match cmd {
                        PtyCommand::Write(data) => write_buf.extend(data),
                        PtyCommand::Resize(size) => pending_resize = Some(size),
                        PtyCommand::Close => {
                            if !closing {
                                pty.start_shutdown();
                            }
                            mark_closing(&mut closing, &mut shutdown_deadline);
                        }
                    }
                }

                if let Some(size) = pending_resize {
                    pty.resize(size);
                }
            }
            x if x == WAIT_OBJECT_0 + 1 => {
                if let Some(child_event) = pty.next_child_event() {
                    child_exited = true;
                    event_tx
                        .send_blocking(PtyEvent::Exited(match child_event {
                            crate::ChildEvent::Exited(s) => s,
                        }))
                        .ok();
                }
            }
            x if x == WAIT_OBJECT_0 + 2 => {
                use windows_sys::Win32::Foundation::ERROR_IO_INCOMPLETE;

                read_pending = false;
                let mut first_output = None;
                let mut read = 0_u32;
                let ok = unsafe { GetOverlappedResult(conout, &mut read_overlapped, &mut read, 0) };
                if ok == 0 {
                    let err = std::io::Error::last_os_error();
                    if err.raw_os_error() == Some(ERROR_IO_INCOMPLETE as i32) {
                        mark_closing(&mut closing, &mut shutdown_deadline);
                    } else {
                        report_non_broken_pipe_error(&event_tx, err);
                        mark_closing(&mut closing, &mut shutdown_deadline);
                    }
                } else if read == 0 {
                    mark_closing(&mut closing, &mut shutdown_deadline);
                } else {
                    first_output = Some(pool.wrap(std::mem::take(&mut read_buf), read as usize));
                    read_buf = pool.acquire();
                }

                let mut second_output = None;
                if first_output.is_some() && !closing {
                    let (pending, output) = queue_read(
                        conout,
                        &mut read_overlapped,
                        &mut read_buf,
                        &pool,
                        &event_tx,
                        &mut closing,
                        &mut shutdown_deadline,
                    );
                    read_pending = pending;
                    second_output = output;
                }

                if let Some(output) = first_output
                    && !emit_output(&event_tx, closing, output)
                {
                    break;
                }
                if let Some(output) = second_output
                    && !emit_output(&event_tx, closing, output)
                {
                    break;
                }
            }
            x if x == WAIT_OBJECT_0 + 3 => {
                write_pending = false;
                if let Err(e) = complete_write(conin, &mut write_overlapped, &mut write_inflight) {
                    report_non_broken_pipe_error(&event_tx, e);
                    mark_closing(&mut closing, &mut shutdown_deadline);
                    write_inflight.clear();
                }
                // write_inflight may still have leftover bytes from a partial write.
                // They'll be retried on the next loop iteration's start_write block.
            }
            WAIT_TIMEOUT => {
                // Keep looping; timeout primarily drives shutdown deadline checks.
            }
            _ => {
                event_tx
                    .send_blocking(PtyEvent::Error(std::io::Error::last_os_error()))
                    .ok();
                break;
            }
        }

        if closing
            && !child_exited
            && let Some(deadline) = shutdown_deadline
            && Instant::now() >= deadline
        {
            pty.force_terminate();
            child_exited = true;
            event_tx.send_blocking(PtyEvent::Exited(None)).ok();
            break;
        }

        if !closing && !read_pending {
            let (pending, output) = queue_read(
                conout,
                &mut read_overlapped,
                &mut read_buf,
                &pool,
                &event_tx,
                &mut closing,
                &mut shutdown_deadline,
            );
            read_pending = pending;
            if let Some(output) = output
                && !emit_output(&event_tx, false, output)
            {
                break;
            }
        }
    }

    // Cancel any pending IO and wait for the kernel to release our buffers.
    if read_pending {
        unsafe { CancelIoEx(conout, &mut read_overlapped as *mut _ as *mut _) };
        let mut _bytes = 0u32;
        unsafe { GetOverlappedResult(conout, &mut read_overlapped, &mut _bytes, 1) }; // bWait=TRUE
    }
    if write_pending {
        unsafe { CancelIoEx(conin, &mut write_overlapped as *mut _ as *mut _) };
        let mut _bytes = 0u32;
        unsafe { GetOverlappedResult(conin, &mut write_overlapped, &mut _bytes, 1) }; // bWait=TRUE
    }

    // Ensure the forwarder can exit even if the UI-side sender is still alive.
    let _ = command_signal_tx.try_send(PtyCommand::Close);

    if let Ok(handle) = cmd_forwarder {
        let _ = handle.join();
    }

    unsafe {
        CloseHandle(write_event);
        CloseHandle(read_event);
        CloseHandle(cmd_event);
    }

    drop(pty);
    if !child_exited {
        event_tx.send_blocking(PtyEvent::Exited(None)).ok();
    }
}

fn mark_closing(closing: &mut bool, shutdown_deadline: &mut Option<Instant>) {
    *closing = true;
    shutdown_deadline.get_or_insert_with(|| Instant::now() + GRACEFUL_SHUTDOWN_TIMEOUT);
}

fn emit_output(event_tx: &Sender<PtyEvent>, closing: bool, data: OutputBuffer) -> bool {
    if closing {
        event_tx.try_send(PtyEvent::Output(data)).ok();
        true
    } else {
        event_tx.send_blocking(PtyEvent::Output(data)).is_ok()
    }
}

fn queue_read(
    conout: windows_sys::Win32::Foundation::HANDLE,
    read_overlapped: &mut windows_sys::Win32::System::IO::OVERLAPPED,
    read_buf: &mut Vec<u8>,
    pool: &std::sync::Arc<BufferPool>,
    event_tx: &Sender<PtyEvent>,
    closing: &mut bool,
    shutdown_deadline: &mut Option<Instant>,
) -> (bool, Option<OutputBuffer>) {
    match start_read(conout, read_overlapped, read_buf) {
        Ok(OverlappedStart::Pending) => (true, None),
        Ok(OverlappedStart::Completed(n)) => {
            if n == 0 {
                mark_closing(closing, shutdown_deadline);
                return (false, None);
            }
            let output = pool.wrap(std::mem::take(read_buf), n);
            *read_buf = pool.acquire();
            (false, Some(output))
        }
        Err(e) => {
            report_non_broken_pipe_error(event_tx, e);
            mark_closing(closing, shutdown_deadline);
            (false, None)
        }
    }
}

fn report_non_broken_pipe_error(event_tx: &Sender<PtyEvent>, e: std::io::Error) -> bool {
    if e.kind() == std::io::ErrorKind::BrokenPipe {
        return true;
    }
    event_tx.send_blocking(PtyEvent::Error(e)).is_ok()
}

enum OverlappedStart {
    Pending,
    Completed(usize),
}

fn start_read(
    conout: windows_sys::Win32::Foundation::HANDLE,
    overlapped: &mut windows_sys::Win32::System::IO::OVERLAPPED,
    buf: &mut [u8],
) -> std::io::Result<OverlappedStart> {
    use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_IO_PENDING};
    use windows_sys::Win32::Storage::FileSystem::ReadFile;
    use windows_sys::Win32::System::Threading::ResetEvent;

    let mut read = 0_u32;
    unsafe {
        ResetEvent(overlapped.hEvent);
        if ReadFile(
            conout,
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
            &mut read,
            overlapped,
        ) == 0
        {
            let err = std::io::Error::last_os_error();
            let raw = err.raw_os_error();
            if raw == Some(ERROR_IO_PENDING as i32) {
                // Overlapped read queued; completion arrives via read_event.
                return Ok(OverlappedStart::Pending);
            }
            if raw == Some(ERROR_BROKEN_PIPE as i32) {
                return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
            }
            return Err(err);
        }
    }
    Ok(OverlappedStart::Completed(read as usize))
}

fn start_write(
    conin: windows_sys::Win32::Foundation::HANDLE,
    overlapped: &mut windows_sys::Win32::System::IO::OVERLAPPED,
    data: &[u8],
) -> std::io::Result<OverlappedStart> {
    use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_IO_PENDING};
    use windows_sys::Win32::Storage::FileSystem::WriteFile;
    use windows_sys::Win32::System::Threading::ResetEvent;

    if data.is_empty() {
        return Ok(OverlappedStart::Completed(0));
    }

    let mut written = 0_u32;
    unsafe {
        ResetEvent(overlapped.hEvent);
        if WriteFile(
            conin,
            data.as_ptr().cast(),
            data.len() as u32,
            &mut written,
            overlapped,
        ) == 0
        {
            let err = std::io::Error::last_os_error();
            let raw = err.raw_os_error();
            if raw == Some(ERROR_IO_PENDING as i32) {
                // Overlapped write queued; completion arrives via write_event.
                return Ok(OverlappedStart::Pending);
            }
            if raw == Some(ERROR_BROKEN_PIPE as i32) {
                return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
            }
            return Err(err);
        }
    }
    Ok(OverlappedStart::Completed(written as usize))
}

fn complete_write(
    conin: windows_sys::Win32::Foundation::HANDLE,
    overlapped: &mut windows_sys::Win32::System::IO::OVERLAPPED,
    inflight: &mut Vec<u8>,
) -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::ERROR_IO_INCOMPLETE;
    use windows_sys::Win32::System::IO::GetOverlappedResult;

    if inflight.is_empty() {
        return Ok(());
    }

    let mut written = 0_u32;
    let ok = unsafe { GetOverlappedResult(conin, overlapped, &mut written, 0) };
    if ok == 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_IO_INCOMPLETE as i32) {
            return Ok(());
        }
        return Err(err);
    }

    let written = written as usize;
    if written >= inflight.len() {
        inflight.clear();
    } else {
        inflight.drain(..written);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use async_channel::TryRecvError;
    use core::option::Option::Some;
    use std::time::Duration;

    use crate::{Options, PtyCommand, PtyEvent, PtyHandle, Shell, WindowSize};

    fn test_window_size() -> WindowSize {
        WindowSize {
            num_lines: 24,
            num_cols: 80,
            cell_width: 8,
            cell_height: 16,
        }
    }

    fn test_options() -> Options {
        Options {
            shell: Some(Shell {
                program: "cmd.exe".into(),
                args: vec![],
            }),
            ..Default::default()
        }
    }

    fn recv_timeout(rx: &async_channel::Receiver<PtyEvent>, timeout: Duration) -> Option<PtyEvent> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match rx.try_recv() {
                Ok(event) => return Some(event),
                Err(TryRecvError::Empty) => {
                    if std::time::Instant::now() >= deadline {
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(TryRecvError::Closed) => return None,
            }
        }
    }

    #[test]
    fn spawn_receives_output() {
        let handle = PtyHandle::spawn(test_options(), test_window_size()).unwrap();

        let event = recv_timeout(&handle.event_rx, Duration::from_secs(5)).unwrap();
        assert!(matches!(
            event,
            PtyEvent::Output(ref data) if !data.is_empty()
        ));
    }

    #[test]
    fn write_and_read_echo() {
        let handle = PtyHandle::spawn(test_options(), test_window_size()).unwrap();

        // Wait for shell prompt.
        std::thread::sleep(Duration::from_millis(500));

        // Drain any initial output.
        while handle.event_rx.try_recv().is_ok() {}

        let cmd = b"echo hello\r\n".to_vec();

        handle
            .command_tx
            .send_blocking(PtyCommand::Write(cmd))
            .unwrap();

        let mut output = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match recv_timeout(&handle.event_rx, Duration::from_millis(100)) {
                Some(PtyEvent::Output(data)) => {
                    output.push_str(&String::from_utf8_lossy(&data));
                    if output.contains("hello") {
                        return; // pass
                    }
                }
                Some(PtyEvent::Exited(_)) => break,
                _ => continue,
            }
        }
        panic!("did not find 'hello' in output. got: {:?}", output);
    }

    #[test]
    fn resize_does_not_crash() {
        let handle = PtyHandle::spawn(test_options(), test_window_size()).unwrap();

        handle
            .command_tx
            .send_blocking(PtyCommand::Resize(WindowSize {
                num_lines: 40,
                num_cols: 120,
                cell_width: 8,
                cell_height: 16,
            }))
            .unwrap();

        std::thread::sleep(Duration::from_millis(100));

        handle.command_tx.send_blocking(PtyCommand::Close).unwrap();
    }

    #[test]
    fn close_emits_exited() {
        let handle = PtyHandle::spawn(test_options(), test_window_size()).unwrap();

        handle.command_tx.send_blocking(PtyCommand::Close).unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match recv_timeout(&handle.event_rx, Duration::from_millis(100)) {
                Some(PtyEvent::Exited(_)) => return,
                Some(_) => continue,
                _ => continue,
            }
        }
        panic!("did not receive Exited event");
    }
}
