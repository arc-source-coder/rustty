use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use polling::{Event, Events, PollMode, Poller};

use crate::{
    Options, PTY_COMMAND_CHANNEL_CAPACITY, PTY_EVENT_CHANNEL_CAPACITY, Pty, PtyCommand, PtyEvent,
    WindowSize,
};

#[cfg(windows)]
use crate::windows::{PTY_CHILD_EVENT_TOKEN, PTY_READ_WRITE_TOKEN};

const READ_BUF_SIZE: usize = 0x10_0000; // 1MB
const SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(100);
#[cfg(windows)]
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

// On unix, define token constants locally (unix backend doesn't
// export them since it doesn't use IOCP).
#[cfg(unix)]
const PTY_READ_WRITE_TOKEN: usize = 0;
#[cfg(unix)]
const PTY_CHILD_EVENT_TOKEN: usize = 1;

pub struct PtyHandle {
    pub event_rx: Receiver<PtyEvent>,
    pub command_tx: SyncSender<PtyCommand>,
    worker: Option<JoinHandle<()>>,
}

impl PtyHandle {
    pub fn spawn(options: Options, window_size: WindowSize) -> std::io::Result<Self> {
        let (event_tx, event_rx) = mpsc::sync_channel(PTY_EVENT_CHANNEL_CAPACITY);
        let (command_tx, command_rx) = mpsc::sync_channel(PTY_COMMAND_CHANNEL_CAPACITY);

        // Spawn the platform PTY before moving to worker thread
        // so we can return spawn errors synchronously.
        let pty = crate::new(&options, window_size)?;

        let worker = thread::Builder::new()
            .name("pty-worker".into())
            .spawn(move || {
                worker_loop(pty, event_tx, command_rx);
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
        let _ = self.command_tx.try_send(PtyCommand::Close);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn worker_loop(mut pty: Pty, event_tx: SyncSender<PtyEvent>, command_rx: Receiver<PtyCommand>) {
    let poller = match Poller::new() {
        Ok(p) => std::sync::Arc::new(p),
        Err(e) => {
            event_tx.send(PtyEvent::Error(e)).ok();
            return;
        }
    };

    // Register PTY for polling.
    #[cfg(windows)]
    {
        let interest = Event::all(PTY_READ_WRITE_TOKEN);
        pty.reader().register(&poller, interest, PollMode::Level);
        pty.writer().register(&poller, interest, PollMode::Level);
        pty.child_watcher()
            .register(&poller, Event::readable(PTY_CHILD_EVENT_TOKEN));
    }

    #[cfg(unix)]
    unsafe {
        let interest = Event::all(PTY_READ_WRITE_TOKEN);
        if let Err(e) = poller.add_with_mode(pty.reader(), interest, PollMode::Level) {
            event_tx.send(PtyEvent::Error(e)).ok();
            return;
        }
    }

    let mut events = Events::new();
    let mut read_buf = vec![0u8; READ_BUF_SIZE];
    let mut write_buf: VecDeque<u8> = VecDeque::new();
    let mut closing = false;
    let mut child_exited = false;
    #[cfg(windows)]
    let mut shutdown_deadline: Option<Instant> = None;

    loop {
        events.clear();

        let timeout = if closing {
            Some(SHUTDOWN_POLL_INTERVAL)
        } else {
            // Wake periodically to check command_rx since we
            // can't add an mpsc receiver to the poller.
            Some(Duration::from_millis(10))
        };

        if let Err(e) = poller.wait(&mut events, timeout) {
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            event_tx.send(PtyEvent::Error(e)).ok();
            break;
        }

        // --- Process poll events ---

        let mut readable = false;
        let mut writable = false;
        let mut child_event = false;

        for event in events.iter() {
            match event.key {
                k if k == PTY_READ_WRITE_TOKEN => {
                    if event.readable {
                        readable = true;
                    }
                    if event.writable {
                        writable = true;
                    }
                }
                k if k == PTY_CHILD_EVENT_TOKEN => {
                    child_event = true;
                }
                _ => {}
            }
        }

        // Always check (poll may have timed out but data could
        // still be available on Windows via try_read).
        readable = true;

        // --- Read output ---

        if readable {
            loop {
                let n = {
                    #[cfg(windows)]
                    {
                        pty.reader().try_read(&mut read_buf)
                    }
                    #[cfg(unix)]
                    {
                        match pty.reader().read(&mut read_buf) {
                            Ok(n) => n,
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => 0,
                            Err(e) => {
                                event_tx.send(PtyEvent::Error(e)).ok();
                                return;
                            }
                        }
                    }
                };

                if n == 0 {
                    break;
                }

                let data = read_buf[..n].to_vec();
                if closing {
                    // During shutdown, don't block on send.
                    event_tx.try_send(PtyEvent::Output(data)).ok();
                } else {
                    if event_tx.send(PtyEvent::Output(data)).is_err() {
                        // Receiver dropped — shut down.
                        return;
                    }
                }
            }
        }

        // --- Write pending data ---

        if writable || !write_buf.is_empty() {
            while !write_buf.is_empty() {
                let (front, _) = write_buf.as_slices();
                if front.is_empty() {
                    break;
                }
                let n = {
                    #[cfg(windows)]
                    {
                        pty.writer().try_write(front)
                    }
                    #[cfg(unix)]
                    {
                        match pty.writer().write(front) {
                            Ok(n) => n,
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => 0,
                            Err(e) => {
                                log::error!("PTY write error: {e}");
                                0
                            }
                        }
                    }
                };
                if n == 0 {
                    break;
                }
                write_buf.drain(..n);
            }
        }

        // --- Check child exit ---

        if (child_event || !child_exited)
            && let Some(child_event) = pty.next_child_event()
        {
            child_exited = true;
            if closing {
                event_tx
                    .try_send(PtyEvent::Exited(match child_event {
                        crate::ChildEvent::Exited(s) => s,
                    }))
                    .ok();
            } else {
                event_tx
                    .send(PtyEvent::Exited(match child_event {
                        crate::ChildEvent::Exited(s) => s,
                    }))
                    .ok();
            }
            // Child exited — begin shutdown.
            closing = true;
        }

        // --- Process commands ---

        let mut pending_resize: Option<WindowSize> = None;
        loop {
            match command_rx.try_recv() {
                Ok(PtyCommand::Write(data)) => {
                    write_buf.extend(&data);
                }
                Ok(PtyCommand::Resize(size)) => {
                    pending_resize = Some(size);
                }
                Ok(PtyCommand::Close) => {
                    if !closing {
                        #[cfg(windows)]
                        {
                            // Close HPCON to send CTRL_CLOSE_EVENT, giving the
                            // child a chance to exit gracefully before we force-kill.
                            pty.start_shutdown();
                            shutdown_deadline = Some(Instant::now() + GRACEFUL_SHUTDOWN_TIMEOUT);
                        }
                    }
                    closing = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !closing {
                        #[cfg(windows)]
                        {
                            pty.start_shutdown();
                            shutdown_deadline = Some(Instant::now() + GRACEFUL_SHUTDOWN_TIMEOUT);
                        }
                    }
                    closing = true;
                    break;
                }
            }
        }

        // Apply coalesced resize (last-wins).
        if let Some(size) = pending_resize {
            pty.resize(size);
        }

        // --- Shutdown ---

        if closing && child_exited {
            // Child has exited and we've been asked to close.
            // Drain any remaining output, then exit.
            break;
        }

        #[cfg(windows)]
        if closing
            && !child_exited
            && let Some(deadline) = shutdown_deadline
            && Instant::now() >= deadline
        {
            // Graceful shutdown timed out — force-kill the child.
            pty.force_terminate();
            shutdown_deadline = None;
        }

        // Re-register for polling on Unix (level-triggered
        // should be automatic, but reregister for edge cases).
        #[cfg(unix)]
        {
            let interest = Event::all(PTY_READ_WRITE_TOKEN);
            let _ = poller.modify_with_mode(pty.reader(), interest, PollMode::Level);
        }

        #[cfg(windows)]
        {
            let interest = Event::all(PTY_READ_WRITE_TOKEN);
            pty.reader().register(&poller, interest, PollMode::Level);
            pty.writer().register(&poller, interest, PollMode::Level);
        }
    }

    // Deregister and drop PTY (triggers ClosePseudoConsole /
    // SIGHUP). conout is still valid at this point because
    // backend is the first field and gets dropped first.
    #[cfg(windows)]
    {
        pty.reader().deregister();
        pty.writer().deregister();
        pty.child_watcher().deregister();
    }

    #[cfg(unix)]
    {
        let _ = poller.delete(pty.reader());
    }

    // pty is dropped here — platform Drop impl handles cleanup.
    drop(pty);

    // If we haven't sent Exited yet, send it now.
    if !child_exited {
        event_tx.try_send(PtyEvent::Exited(None)).ok();
    }
}

#[cfg(test)]
mod tests {
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
                #[cfg(windows)]
                program: "powershell.exe".into(),
                #[cfg(unix)]
                program: "/bin/sh".into(),
                args: vec![],
            }),
            ..Default::default()
        }
    }

    #[test]
    fn spawn_receives_output() {
        let handle = PtyHandle::spawn(test_options(), test_window_size()).unwrap();

        let event = handle
            .event_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
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

        #[cfg(windows)]
        let cmd = b"echo hello\r\n".to_vec();
        #[cfg(unix)]
        let cmd = b"echo hello\n".to_vec();

        handle.command_tx.send(PtyCommand::Write(cmd)).unwrap();

        let mut output = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match handle.event_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(PtyEvent::Output(data)) => {
                    output.push_str(&String::from_utf8_lossy(&data));
                    if output.contains("hello") {
                        return; // pass
                    }
                }
                Ok(PtyEvent::Exited(_)) => break,
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
            .send(PtyCommand::Resize(WindowSize {
                num_lines: 40,
                num_cols: 120,
                cell_width: 8,
                cell_height: 16,
            }))
            .unwrap();

        std::thread::sleep(Duration::from_millis(100));

        handle.command_tx.send(PtyCommand::Close).unwrap();
    }

    #[test]
    fn close_emits_exited() {
        let handle = PtyHandle::spawn(test_options(), test_window_size()).unwrap();

        handle.command_tx.send(PtyCommand::Close).unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match handle.event_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(PtyEvent::Exited(_)) => return,
                Ok(_) => continue,
                Err(_) => continue,
            }
        }
        panic!("did not receive Exited event");
    }
}
