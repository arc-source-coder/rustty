use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender, TryRecvError};

use ghostty_vt::{Terminal, VtEvent};
use pty::{OutputBuffer, PtyCommand, PtyEvent};

use crate::types::{IoEvent, SideEffect};

/// Maximum time the IO thread holds the terminal lock before yielding.
/// Prevents UI thread starvation under heavy output.
const IO_LOCK_BUDGET: Duration = Duration::from_millis(4);

/// Timeout for synchronized output safety timer.
/// Matches Ghostty's 1-second timeout.
const SYNC_OUTPUT_TIMEOUT: Duration = Duration::from_secs(1);

/// Spawn the IO thread. Returns the JoinHandle.
///
/// The IO thread drains PtyEvent::Output from `pty_event_rx`, feeds bytes
/// to the terminal (behind `terminal_mutex`), processes side effects and
/// device responses, and signals the UI thread when rendering is needed.
///
/// # Arguments
/// - `terminal`: shared terminal state
/// - `pty_event_rx`: receives PTY output + lifecycle events
/// - `pty_command_tx`: sends device responses back to PTY
/// - `signal_tx`: capacity-1 channel to wake UI thread for rendering
/// - `event_tx`: bounded(64) channel for side effects + process lifecycle
pub fn spawn(
    terminal: Arc<Mutex<Terminal>>,
    pty_event_rx: Receiver<PtyEvent>,
    pty_command_tx: Sender<PtyCommand>,
    signal_tx: Sender<()>,
    event_tx: Sender<IoEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("io-thread".into())
        .spawn(move || {
            io_loop(terminal, pty_event_rx, pty_command_tx, signal_tx, event_tx);
        })
        .expect("failed to spawn IO thread")
}

fn io_loop(
    terminal: Arc<Mutex<Terminal>>,
    pty_event_rx: Receiver<PtyEvent>,
    pty_command_tx: Sender<PtyCommand>,
    signal_tx: Sender<()>,
    event_tx: Sender<IoEvent>,
) {
    let mut sync_output_since: Option<Instant> = None;

    loop {
        // Block until first event arrives.
        let first = match pty_event_rx.recv_blocking() {
            Ok(event) => event,
            Err(_) => break, // Channel closed
        };

        // Handle non-output events (exit, error) without locking terminal.
        match first {
            PtyEvent::Output(bytes) => {
                let (vt_events, sync_active) =
                    feed_with_budget(&terminal, bytes, &pty_event_rx, &event_tx);
                process_vt_events(vt_events, &pty_command_tx, &event_tx);

                if sync_active {
                    let since = *sync_output_since.get_or_insert_with(Instant::now);
                    // Safety timer: force signal after 1s.
                    if since.elapsed() >= SYNC_OUTPUT_TIMEOUT {
                        sync_output_since = None;
                        signal_tx.try_send(()).ok();
                    }
                    // Otherwise: defer signaling (don't render partial frames).
                } else {
                    sync_output_since = None;
                    signal_tx.try_send(()).ok();
                }
            }
            PtyEvent::Exited(status) => {
                // Drain remaining output before reporting exit.
                drain_remaining_output(&terminal, &pty_event_rx, &pty_command_tx, &event_tx);
                // send_blocking since the Exited event should not be lost
                event_tx
                    .send_blocking(IoEvent::Exited(status))
                    .expect("event channel closed before exit event could be sent");
                signal_tx.try_send(()).ok();
                break;
            }
            PtyEvent::Error(err) => {
                // send_blocking since the Error event should not be lost
                event_tx
                    .send_blocking(IoEvent::Error(err.to_string()))
                    .expect("event channel closed before error event could be sent");
                signal_tx.try_send(()).ok();
                break;
            }
        }
    }
}

/// Lock terminal, feed bytes, continue draining while within budget.
/// Returns all VtEvents produced during feeding, and whether synchronized
/// output mode is active — folding both reads into one lock scope so the
/// caller doesn't need a second lock acquisition.
fn feed_with_budget(
    terminal: &Mutex<Terminal>,
    first_bytes: OutputBuffer,
    pty_event_rx: &Receiver<PtyEvent>,
    event_tx: &Sender<IoEvent>,
) -> (Vec<VtEvent>, bool) {
    let deadline = Instant::now() + IO_LOCK_BUDGET;
    let mut term = terminal.lock().expect("terminal mutex poisoned");

    term.feed(&first_bytes);

    // Continue draining while within budget.
    while Instant::now() < deadline {
        match pty_event_rx.try_recv() {
            Ok(PtyEvent::Output(bytes)) => {
                term.feed(&bytes);
            }
            Ok(PtyEvent::Exited(status)) => {
                // Finish current batch, then report exit.
                let events = term.drain_events();
                drop(term);
                // send_blocking since the Exited event should not be lost
                event_tx
                    .send_blocking(IoEvent::Exited(status))
                    .expect("event channel closed before exit event could be sent");
                return (events, false);
            }
            Ok(PtyEvent::Error(err)) => {
                let events = term.drain_events();
                drop(term);
                // send_blocking since the Error event should not be lost
                event_tx
                    .send_blocking(IoEvent::Error(err.to_string()))
                    .expect("event channel closed before error event could be sent");
                return (events, false);
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Closed) => break,
        }
    }

    let sync_active = term.is_synchronized_output();
    (term.drain_events(), sync_active)
    // term lock released here
}

/// Process VtEvents produced during feeding:
/// - DeviceResponse → send to PTY immediately
/// - Bell/Title → send to UI via event channel
fn process_vt_events(
    events: Vec<VtEvent>,
    pty_command_tx: &Sender<PtyCommand>,
    event_tx: &Sender<IoEvent>,
) {
    for event in events {
        match event {
            VtEvent::DeviceResponse(bytes) => {
                pty_command_tx.try_send(PtyCommand::Write(bytes)).ok();
            }
            VtEvent::Bell => {
                if event_tx
                    .try_send(IoEvent::SideEffect(SideEffect::Bell))
                    .is_err()
                {
                    log::warn!("IO event channel full, dropping Bell side effect");
                }
            }
            VtEvent::TitleChanged(title) => {
                if event_tx
                    .try_send(IoEvent::SideEffect(SideEffect::TitleChanged(title)))
                    .is_err()
                {
                    log::warn!("IO event channel full, dropping Title side effect");
                }
            }
        }
    }
}

/// Drain all remaining output from the PTY channel before exit.
/// Called when we receive Exited/Error to ensure no data loss.
fn drain_remaining_output(
    terminal: &Mutex<Terminal>,
    pty_event_rx: &Receiver<PtyEvent>,
    pty_command_tx: &Sender<PtyCommand>,
    event_tx: &Sender<IoEvent>,
) {
    loop {
        match pty_event_rx.try_recv() {
            Ok(PtyEvent::Output(bytes)) => {
                let events = {
                    let mut term = terminal.lock().expect("terminal mutex poisoned");
                    term.feed(&bytes);
                    term.drain_events()
                };
                process_vt_events(events, pty_command_tx, event_tx);
            }
            Ok(_) => {} // Ignore duplicate exit/error
            Err(_) => break,
        }
    }
}
