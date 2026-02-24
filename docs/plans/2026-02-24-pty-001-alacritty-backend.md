# pty-001: Replace portable-pty with Alacritty's PTY Backend

**Goal:** Replace `portable-pty` with alacritty_terminal's PTY backend code (ConPTY on Windows, openpty on Unix), `cfg`-gated for cross-platform support. Wrap with typed `PtyCommand`/`PtyEvent` channel protocol.

**Architecture:** Copy alacritty_terminal's `tty/` module (~580 lines across 5 files) into `crates/pty/src/`, adapting it to emit our `PtyCommand`/`PtyEvent` channel protocol rather than alacritty's `EventedReadWrite` trait. The Windows backend uses ConPTY via `windows-sys` (pure Rust FFI bindings, no C/C++ build step). The Unix backend uses `rustix-openpty`. Both are `cfg`-gated at the module level. A poll-driven `PtyHandle` adapter wraps the platform `Pty` in a single worker thread with bounded channels.

**Tech Stack:** `windows-sys 0.59` + `miow` + `piper` (Windows), `rustix-openpty` + `signal-hook` + `libc` (Unix), `polling` (both), `log` (both)

**Refs:**

- `opensrc/packages/alacritty/alacritty/alacritty_terminal/src/tty/` — source code to adapt
- `docs/architecture/04-pty-threading.md` — our channel design, shutdown ordering, write ordering
- `docs/architecture/02-data-model.md` § PTY Channel Bounds, § PTY Model
- `vendor/zed/crates/terminal` — Zed's usage of alacritty_terminal (shows the pattern works)

---

## Decision Record

### Problem

`portable-pty` has reliability issues on Windows — ConPTY interactions are buggy, and the crate's abstraction layer adds complexity without adding value for our use case. The next alternative, `winpty-rs`, pulls in a C++ library and complicates the build.

### Decision

Adopt alacritty_terminal's PTY backend code directly into `crates/pty`. This code:

1. **Uses ConPTY directly** via `windows-sys` — pure Rust FFI, no C/C++ build step
2. **Supports `conpty.dll` from Windows Terminal** — tries loading the improved OpenConsole implementation first, falls back to system ConPTY (the `ConptyApi` struct handles this transparently)
3. **Is battle-tested** — ships in both Alacritty and Zed to Windows/macOS/Linux users
4. **Has clean platform separation** — `#[cfg(windows)]` and `#[cfg(unix)]` at the module level, no runtime branching

### What is Windows Terminal's `conpty.dll`?

Windows ships a built-in ConPTY API (`CreatePseudoConsole` in `kernel32.dll`). The Windows Terminal team also ships a standalone `conpty.dll` bundled with `OpenConsole.exe` — a significantly improved implementation with better VT sequence handling, fewer bugs, and better performance. Same API surface, better implementation. Alacritty's code probes for this DLL first (in PATH and next to the executable), and falls back to the system API if not found. We get this for free.

### Cross-Platform Rationale

We include the Unix (macOS/Linux) backend as well, `cfg`-gated so it compiles to nothing on Windows. The cost is near-zero (~440 lines behind `#[cfg(unix)]`), and it means:

- We don't paint ourselves into a Windows-only corner
- CI can validate on all platforms from day one
- Every other layer (GPUI, ghostty-vt, terminal session, renderer) is already cross-platform

### Dependencies and Version Alignment

**Windows** (`[target.'cfg(windows)'.dependencies]`):

- `windows-sys 0.59` — Win32 API bindings (ConPTY, process creation, threading). We use 0.59 because `miow 0.6.0` depends on `^0.48`, and 0.59 is already in our lockfile. The lockfile also has 0.61 (via `polling`, GPUI), but Cargo deduplicates correctly — `windows-sys` is just `extern "system" fn` declarations (zero codegen), so having two versions adds negligible compile time and zero binary size. If `miow` upgrades to require `>=0.60` we can bump.
- `miow 0.6.0` — anonymous pipe pairs for ConPTY stdin/stdout
- `piper` — in-process async pipe bridging (blocking reader/writer thread ↔ polling)

**Unix** (`[target.'cfg(unix)'.dependencies]`):

- `libc` — POSIX syscalls (`setsid`, `ioctl`, `kill`, signal constants) used in `pre_exec` and resize
- `rustix-openpty` — PTY allocation (replaces libc `openpty`)
- `rustix` — terminal attributes (IUTF8, winsize)
- `signal-hook` — SIGCHLD notification

**Both**:

- `polling` — cross-platform event notification (epoll on Linux, kqueue on macOS, IOCP on Windows)
- `log` — error/info logging

### What We Don't Take

- `EventedReadWrite` / `EventedPty` traits — we use channels, not poll-based I/O traits
- `setup_env()` / `terminfo_exists()` — we set TERM/COLORTERM from our `SpawnConfig`, not via a global function
- Alacritty-specific env vars (`ALACRITTY_WINDOW_ID`, `WINDOWID`)

---

## Implementation Plan

### File Inventory

After implementation, `crates/pty/src/` will contain:

```text
crates/pty/
├── Cargo.toml
└── src/
    ├── lib.rs              (shared types + cfg re-exports)
    ├── handle.rs           (PtyHandle channel adapter)
    ├── unix.rs             (Unix PTY backend, cfg(unix))
    └── windows/
        ├── mod.rs          (Windows Pty struct + helpers)
        ├── blocking.rs     (UnblockedReader/Writer)
        ├── child.rs        (ChildExitWatcher via IOCP)
        └── conpty.rs       (ConPTY creation + resize)
```

Note: Zed's AGENTS.md says "never create files with `mod.rs` paths", but
that convention is for Zed's codebase. Our project uses `mod.rs` (see
existing code). The `windows/mod.rs` here is the natural pattern for a
platform-specific sub-module with multiple files.

---

### Task 1: Cargo.toml + `lib.rs` with All Shared Types

**Files:**

- Overwrite: `crates/pty/Cargo.toml`
- Delete: `crates/pty/src/main.rs`
- Create: `crates/pty/src/lib.rs`

Types must be defined first — Tasks 2–4 all reference them.

#### `crates/pty/Cargo.toml`

```toml
[package]
name = "pty"
version.workspace = true
edition.workspace = true

[dependencies]
log = "0.4"
polling = "3.8"

[target.'cfg(unix)'.dependencies]
libc = "0.2"
rustix-openpty = "0.2.0"
rustix = { version = "1.0.0", default-features = false, features = ["std"] }
signal-hook = "0.3.10"

[target.'cfg(windows)'.dependencies]
piper = "0.2.1"
miow = "0.6.0"
windows-sys = { version = "0.59", features = [
    "Win32_System_Console",
    "Win32_System_IO",
    "Win32_Foundation",
    "Win32_Security",
    "Win32_System_LibraryLoader",
    "Win32_System_Threading",
    "Win32_System_WindowsProgramming",
] }
```

#### `crates/pty/src/lib.rs`

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitStatus;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use self::unix::Pty;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::Pty;

mod handle;
pub use handle::PtyHandle;

// --- Shared types used by both platform backends ---

#[derive(Debug, PartialEq, Eq)]
pub enum ChildEvent {
    Exited(Option<ExitStatus>),
}

#[derive(Clone, Copy, Debug)]
pub struct WindowSize {
    pub num_lines: u16,
    pub num_cols: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

#[derive(Clone, Debug, Default)]
pub struct Shell {
    pub program: String,
    pub args: Vec<String>,
}

impl Shell {
    pub fn new(program: String, args: Vec<String>) -> Self {
        Self { program, args }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub shell: Option<Shell>,
    pub working_directory: Option<PathBuf>,
    pub env: HashMap<String, String>,
    #[cfg(windows)]
    pub escape_args: bool,
}

// --- Channel protocol ---

pub const PTY_EVENT_CHANNEL_CAPACITY: usize = 64;
pub const PTY_COMMAND_CHANNEL_CAPACITY: usize = 64;

pub enum PtyCommand {
    Write(Vec<u8>),
    Resize(WindowSize),
    Close,
}

pub enum PtyEvent {
    Output(Vec<u8>),
    Exited(Option<ExitStatus>),
    Error(std::io::Error),
}
```

**Verify:** Create stub files (`crates/pty/src/handle.rs`, `crates/pty/src/unix.rs`, `crates/pty/src/windows/mod.rs`) so `cargo check -p pty` passes. Delete `crates/pty/src/main.rs`.

---

### Task 2: Windows Backend (all 4 files)

Port as a single unit — these files are tightly coupled via drop ordering, type aliases, and internal threading.

**Source:** `opensrc/packages/alacritty/alacritty/alacritty_terminal/src/tty/windows/`

#### `crates/pty/src/windows/blocking.rs`

Copied from alacritty with one change: replace `crate::thread::spawn_named` with inline `thread::Builder`.

```rust
//! Bridges blocking pipe I/O to `polling` via in-process `piper` pipes.
//! Each UnblockedReader/Writer spawns a background thread that does the actual
//! blocking I/O, communicating with the foreground via a piper pipe. The
//! Registration Wake impl posts IOCP completion packets to notify the poller.

use std::io::prelude::*;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::{io, thread};

use piper::{Reader, Writer, pipe};
use polling::os::iocp::{CompletionPacket, PollerIocpExt};
use polling::{Event, PollMode, Poller};

struct Registration {
    interest: Mutex<Option<Interest>>,
    end: PipeEnd,
}

#[derive(Copy, Clone)]
enum PipeEnd {
    Reader,
    Writer,
}

struct Interest {
    event: Event,
    poller: Arc<Poller>,
    mode: PollMode,
}

fn spawn_named<F, T, S>(name: S, f: F) -> thread::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
    S: Into<String>,
{
    thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .expect("thread spawn failed")
}

pub struct UnblockedReader<R> {
    interest: Arc<Registration>,
    pipe: Reader,
    first_register: bool,
    _reader: PhantomData<R>,
}

impl<R: Read + Send + 'static> UnblockedReader<R> {
    pub fn new(mut source: R, pipe_capacity: usize) -> Self {
        let (reader, mut writer) = pipe(pipe_capacity);
        let interest = Arc::new(Registration {
            interest: Mutex::<Option<Interest>>::new(None),
            end: PipeEnd::Reader,
        });

        spawn_named("pty-reader-thread", move || {
            let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
            let mut context = Context::from_waker(&waker);

            loop {
                match writer.poll_fill(&mut context, &mut source) {
                    Poll::Ready(Ok(0)) => return,
                    Poll::Ready(Ok(_)) => continue,
                    Poll::Ready(Err(e))
                        if e.kind() == io::ErrorKind::Interrupted =>
                    {
                        continue;
                    }
                    Poll::Ready(Err(e)) => {
                        log::error!("error writing to pipe: {}", e);
                        return;
                    }
                    Poll::Pending => thread::park(),
                }
            }
        });

        Self {
            interest,
            pipe: reader,
            first_register: true,
            _reader: PhantomData,
        }
    }

    pub fn register(
        &mut self,
        poller: &Arc<Poller>,
        event: Event,
        mode: PollMode,
    ) {
        let mut interest = self.interest.interest.lock().unwrap();
        *interest =
            Some(Interest { event, poller: poller.clone(), mode });

        if (!self.pipe.is_empty() && event.readable)
            || self.first_register
        {
            self.first_register = false;
            poller.post(CompletionPacket::new(event)).ok();
        }
    }

    pub fn deregister(&self) {
        let mut interest = self.interest.interest.lock().unwrap();
        *interest = None;
    }

    pub fn try_read(&mut self, buf: &mut [u8]) -> usize {
        let waker = Waker::from(self.interest.clone());
        match self
            .pipe
            .poll_drain_bytes(&mut Context::from_waker(&waker), buf)
        {
            Poll::Pending => 0,
            Poll::Ready(n) => n,
        }
    }
}

impl<R: Read + Send + 'static> Read for UnblockedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Ok(self.try_read(buf))
    }
}

pub struct UnblockedWriter<W> {
    interest: Arc<Registration>,
    pipe: Writer,
    _writer: PhantomData<W>,
}

impl<W: Write + Send + 'static> UnblockedWriter<W> {
    pub fn new(mut sink: W, pipe_capacity: usize) -> Self {
        let (mut reader, writer) = pipe(pipe_capacity);
        let interest = Arc::new(Registration {
            interest: Mutex::<Option<Interest>>::new(None),
            end: PipeEnd::Writer,
        });

        spawn_named("pty-writer-thread", move || {
            let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
            let mut context = Context::from_waker(&waker);

            loop {
                match reader.poll_drain(&mut context, &mut sink) {
                    Poll::Ready(Ok(0)) => return,
                    Poll::Ready(Ok(_)) => continue,
                    Poll::Ready(Err(e))
                        if e.kind() == io::ErrorKind::Interrupted =>
                    {
                        continue;
                    }
                    Poll::Ready(Err(e)) => {
                        log::error!("error writing to sink: {}", e);
                        return;
                    }
                    Poll::Pending => thread::park(),
                }
            }
        });

        Self { interest, pipe: writer, _writer: PhantomData }
    }

    pub fn register(
        &self,
        poller: &Arc<Poller>,
        event: Event,
        mode: PollMode,
    ) {
        let mut interest = self.interest.interest.lock().unwrap();
        *interest =
            Some(Interest { event, poller: poller.clone(), mode });

        if !self.pipe.is_full() && event.writable {
            poller.post(CompletionPacket::new(event)).ok();
        }
    }

    pub fn deregister(&self) {
        let mut interest = self.interest.interest.lock().unwrap();
        *interest = None;
    }

    pub fn try_write(&mut self, buf: &[u8]) -> usize {
        let waker = Waker::from(self.interest.clone());
        match self
            .pipe
            .poll_fill_bytes(&mut Context::from_waker(&waker), buf)
        {
            Poll::Pending => 0,
            Poll::Ready(n) => n,
        }
    }
}

impl<W: Write + Send + 'static> Write for UnblockedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(self.try_write(buf))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct ThreadWaker(thread::Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

impl Wake for Registration {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let mut interest_lock = self.interest.lock().unwrap();
        if let Some(interest) = interest_lock.as_ref() {
            let send_event = match self.end {
                PipeEnd::Reader => interest.event.readable,
                PipeEnd::Writer => interest.event.writable,
            };

            if send_event {
                interest
                    .poller
                    .post(CompletionPacket::new(interest.event))
                    .ok();

                if matches!(
                    interest.mode,
                    PollMode::Oneshot | PollMode::EdgeOneshot
                ) {
                    *interest_lock = None;
                }
            }
        }
    }
}
```

#### `crates/pty/src/windows/child.rs`

Copied from alacritty. Only import path changed: `crate::tty::ChildEvent` → `crate::ChildEvent`.

```rust
use std::ffi::c_void;
use std::io::Error;
use std::num::NonZeroU32;
use std::os::windows::process::ExitStatusExt;
use std::process::ExitStatus;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use polling::os::iocp::{CompletionPacket, PollerIocpExt};
use polling::{Event, Poller};

use windows_sys::Win32::Foundation::{BOOLEAN, FALSE, HANDLE};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessId, INFINITE,
    RegisterWaitForSingleObject, UnregisterWait,
    WT_EXECUTEINWAITTHREAD, WT_EXECUTEONLYONCE,
};

use crate::ChildEvent;

struct Interest {
    poller: Arc<Poller>,
    event: Event,
}

struct ChildExitSender {
    sender: mpsc::Sender<ChildEvent>,
    interest: Arc<Mutex<Option<Interest>>>,
    child_handle: AtomicPtr<c_void>,
}

extern "system" fn child_exit_callback(
    ctx: *mut c_void,
    timed_out: BOOLEAN,
) {
    if timed_out != 0 {
        return;
    }

    let event_tx: Box<_> =
        unsafe { Box::from_raw(ctx as *mut ChildExitSender) };

    let mut exit_code = 0_u32;
    let child_handle =
        event_tx.child_handle.load(Ordering::Relaxed) as HANDLE;
    let status =
        unsafe { GetExitCodeProcess(child_handle, &mut exit_code) };
    let exit_status = if status == FALSE {
        None
    } else {
        Some(ExitStatus::from_raw(exit_code))
    };
    event_tx.sender.send(ChildEvent::Exited(exit_status)).ok();

    let interest = event_tx.interest.lock().unwrap();
    if let Some(interest) = interest.as_ref() {
        interest
            .poller
            .post(CompletionPacket::new(interest.event))
            .ok();
    }
}

pub struct ChildExitWatcher {
    wait_handle: AtomicPtr<c_void>,
    event_rx: mpsc::Receiver<ChildEvent>,
    interest: Arc<Mutex<Option<Interest>>>,
    child_handle: AtomicPtr<c_void>,
    pid: Option<NonZeroU32>,
}

impl ChildExitWatcher {
    pub fn new(
        child_handle: HANDLE,
    ) -> std::io::Result<ChildExitWatcher> {
        let (event_tx, event_rx) = mpsc::channel();

        let mut wait_handle: HANDLE = ptr::null_mut();
        let interest = Arc::new(Mutex::new(None));
        let sender_ref = Box::new(ChildExitSender {
            sender: event_tx,
            interest: interest.clone(),
            child_handle: AtomicPtr::from(child_handle),
        });

        let success = unsafe {
            RegisterWaitForSingleObject(
                &mut wait_handle,
                child_handle,
                Some(child_exit_callback),
                Box::into_raw(sender_ref).cast(),
                INFINITE,
                WT_EXECUTEINWAITTHREAD | WT_EXECUTEONLYONCE,
            )
        };

        if success == 0 {
            Err(Error::last_os_error())
        } else {
            let pid =
                unsafe { NonZeroU32::new(GetProcessId(child_handle)) };
            Ok(ChildExitWatcher {
                event_rx,
                interest,
                pid,
                child_handle: AtomicPtr::from(child_handle),
                wait_handle: AtomicPtr::from(wait_handle),
            })
        }
    }

    pub fn event_rx(&self) -> &mpsc::Receiver<ChildEvent> {
        &self.event_rx
    }

    pub fn register(&self, poller: &Arc<Poller>, event: Event) {
        *self.interest.lock().unwrap() =
            Some(Interest { poller: poller.clone(), event });
    }

    pub fn deregister(&self) {
        *self.interest.lock().unwrap() = None;
    }

    pub fn pid(&self) -> Option<NonZeroU32> {
        self.pid
    }
}

impl Drop for ChildExitWatcher {
    fn drop(&mut self) {
        unsafe {
            UnregisterWait(
                self.wait_handle.load(Ordering::Relaxed) as HANDLE,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::windows::io::AsRawHandle;
    use std::process::Command;
    use std::sync::Arc;
    use std::time::Duration;

    use super::super::PTY_CHILD_EVENT_TOKEN;
    use super::*;

    #[test]
    pub fn event_is_emitted_when_child_exits() {
        const WAIT_TIMEOUT: Duration = Duration::from_millis(200);

        let poller = Arc::new(Poller::new().unwrap());

        let mut child = Command::new("cmd.exe").spawn().unwrap();
        let child_exit_watcher =
            ChildExitWatcher::new(child.as_raw_handle() as HANDLE)
                .unwrap();
        child_exit_watcher
            .register(&poller, Event::readable(PTY_CHILD_EVENT_TOKEN));

        child.kill().unwrap();

        let mut events = polling::Events::new();
        poller.wait(&mut events, Some(WAIT_TIMEOUT)).unwrap();
        assert_eq!(
            events.iter().next().unwrap().key,
            PTY_CHILD_EVENT_TOKEN
        );
        let expected_status = ExitStatus::from_raw(1);
        assert_eq!(
            child_exit_watcher.event_rx().try_recv(),
            Ok(ChildEvent::Exited(Some(expected_status)))
        );
    }
}
```

#### `crates/pty/src/windows/conpty.rs`

Adapted from alacritty. Changes:

- Imports point to `crate::*` and `super::*` instead of `crate::event`/`crate::tty`
- `PIPE_CAPACITY` is a local const (1MB)
- `assert_eq!(result, S_OK)` replaced with proper error return
- `OnResize` trait replaced with a direct `resize` method on `Conpty`

```rust
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::io::{Error, ErrorKind, Result};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::IntoRawHandle;
use std::{mem, ptr};

use log::{info, warn};
use windows_sys::Win32::Foundation::{HANDLE, S_OK};
use windows_sys::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON,
    ResizePseudoConsole,
};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryW,
};
use windows_sys::core::{HRESULT, PWSTR};
use windows_sys::{s, w};

use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    EXTENDED_STARTUPINFO_PRESENT,
    InitializeProcThreadAttributeList,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW,
    UpdateProcThreadAttribute,
};

use super::blocking::{UnblockedReader, UnblockedWriter};
use super::child::ChildExitWatcher;
use super::{Pty, cmdline, win32_string};
use crate::{Options, WindowSize};

const PIPE_CAPACITY: usize = 0x10_0000; // 1MB

type CreatePseudoConsoleFn = unsafe extern "system" fn(
    COORD,
    HANDLE,
    HANDLE,
    u32,
    *mut HPCON,
) -> HRESULT;
type ResizePseudoConsoleFn =
    unsafe extern "system" fn(HPCON, COORD) -> HRESULT;
type ClosePseudoConsoleFn = unsafe extern "system" fn(HPCON);

struct ConptyApi {
    create: CreatePseudoConsoleFn,
    resize: ResizePseudoConsoleFn,
    close: ClosePseudoConsoleFn,
}

impl ConptyApi {
    fn new() -> Self {
        match Self::load_conpty() {
            Some(conpty) => {
                info!("Using conpty.dll for pseudoconsole");
                conpty
            }
            None => {
                info!("Using Windows API for pseudoconsole");
                Self {
                    create: CreatePseudoConsole,
                    resize: ResizePseudoConsole,
                    close: ClosePseudoConsole,
                }
            }
        }
    }

    fn load_conpty() -> Option<Self> {
        type LoadedFn = unsafe extern "system" fn() -> isize;
        unsafe {
            let hmodule = LoadLibraryW(w!("conpty.dll"));
            if hmodule.is_null() {
                return None;
            }
            let create_fn =
                GetProcAddress(hmodule, s!("CreatePseudoConsole"))?;
            let resize_fn =
                GetProcAddress(hmodule, s!("ResizePseudoConsole"))?;
            let close_fn =
                GetProcAddress(hmodule, s!("ClosePseudoConsole"))?;

            Some(Self {
                create: mem::transmute::<
                    LoadedFn,
                    CreatePseudoConsoleFn,
                >(create_fn),
                resize: mem::transmute::<
                    LoadedFn,
                    ResizePseudoConsoleFn,
                >(resize_fn),
                close: mem::transmute::<
                    LoadedFn,
                    ClosePseudoConsoleFn,
                >(close_fn),
            })
        }
    }
}

/// RAII Pseudoconsole handle.
pub struct Conpty {
    pub handle: HPCON,
    api: ConptyApi,
}

impl Conpty {
    pub fn resize(&mut self, window_size: WindowSize) {
        let result =
            unsafe { (self.api.resize)(self.handle, window_size.into()) };
        if result != S_OK {
            log::error!(
                "ResizePseudoConsole failed: HRESULT 0x{:08X}",
                result
            );
        }
    }
}

impl Drop for Conpty {
    fn drop(&mut self) {
        // This blocks until conout pipe is drained. Will deadlock if
        // conout pipe has already been dropped — field ordering in Pty
        // ensures backend is dropped first.
        unsafe { (self.api.close)(self.handle) }
    }
}

unsafe impl Send for Conpty {}

pub fn new(config: &Options, window_size: WindowSize) -> Result<Pty> {
    let api = ConptyApi::new();
    let mut pty_handle: HPCON = 0;

    let (conout, conout_pty_handle) = miow::pipe::anonymous(0)?;
    let (conin_pty_handle, conin) = miow::pipe::anonymous(0)?;

    let result = unsafe {
        (api.create)(
            window_size.into(),
            conin_pty_handle.into_raw_handle() as HANDLE,
            conout_pty_handle.into_raw_handle() as HANDLE,
            0,
            &mut pty_handle as *mut _,
        )
    };

    if result != S_OK {
        return Err(Error::new(
            ErrorKind::Unsupported,
            format!(
                "CreatePseudoConsole failed (HRESULT 0x{:08X}). \
                 Requires Windows 10 1809+.",
                result,
            ),
        ));
    }

    let mut success;
    let mut size: usize = 0;

    let mut startup_info_ex: STARTUPINFOEXW =
        unsafe { mem::zeroed() };
    startup_info_ex.StartupInfo.lpTitle =
        std::ptr::null_mut() as PWSTR;
    startup_info_ex.StartupInfo.cb =
        mem::size_of::<STARTUPINFOEXW>() as u32;

    // Prevents the PTY process from inheriting any handles.
    startup_info_ex.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;

    unsafe {
        let failure = InitializeProcThreadAttributeList(
            ptr::null_mut(),
            1,
            0,
            &mut size as *mut usize,
        ) > 0;

        if failure {
            return Err(Error::last_os_error());
        }
    }

    let mut attr_list: Box<[u8]> =
        vec![0; size].into_boxed_slice();

    #[allow(clippy::cast_ptr_alignment)]
    {
        startup_info_ex.lpAttributeList =
            attr_list.as_mut_ptr() as _;
    }

    unsafe {
        success = InitializeProcThreadAttributeList(
            startup_info_ex.lpAttributeList,
            1,
            0,
            &mut size as *mut usize,
        ) > 0;

        if !success {
            return Err(Error::last_os_error());
        }
    }

    unsafe {
        success = UpdateProcThreadAttribute(
            startup_info_ex.lpAttributeList,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            pty_handle as *mut std::ffi::c_void,
            mem::size_of::<HPCON>(),
            ptr::null_mut(),
            ptr::null_mut(),
        ) > 0;

        if !success {
            return Err(Error::last_os_error());
        }
    }

    let cmdline = win32_string(&cmdline(config));
    let cwd = config.working_directory.as_ref().map(win32_string);
    let mut creation_flags = EXTENDED_STARTUPINFO_PRESENT;
    let custom_env_block = convert_custom_env(&config.env);
    let custom_env_block_pointer = match &custom_env_block {
        Some(custom_env_block) => {
            creation_flags |= CREATE_UNICODE_ENVIRONMENT;
            custom_env_block.as_ptr() as *mut std::ffi::c_void
        }
        None => ptr::null_mut(),
    };

    let mut proc_info: PROCESS_INFORMATION =
        unsafe { mem::zeroed() };
    unsafe {
        success = CreateProcessW(
            ptr::null(),
            cmdline.as_ptr() as PWSTR,
            ptr::null_mut(),
            ptr::null_mut(),
            false as i32,
            creation_flags,
            custom_env_block_pointer,
            cwd.as_ref()
                .map_or_else(ptr::null, |s| s.as_ptr()),
            &mut startup_info_ex.StartupInfo
                as *mut STARTUPINFOW,
            &mut proc_info as *mut PROCESS_INFORMATION,
        ) > 0;

        if !success {
            return Err(Error::last_os_error());
        }
    }

    let conin = UnblockedWriter::new(conin, PIPE_CAPACITY);
    let conout = UnblockedReader::new(conout, PIPE_CAPACITY);

    let child_watcher =
        ChildExitWatcher::new(proc_info.hProcess)?;
    let conpty =
        Conpty { handle: pty_handle as HPCON, api };

    Ok(Pty::new(conpty, conout, conin, child_watcher))
}

fn convert_custom_env(
    custom_env: &HashMap<String, String>,
) -> Option<Vec<u16>> {
    if custom_env.is_empty() {
        return None;
    }

    let mut converted_block = Vec::new();
    let mut all_env_keys = HashSet::new();
    for (custom_key, custom_value) in custom_env {
        let custom_key_os = OsStr::new(custom_key);
        if all_env_keys.insert(custom_key_os.to_ascii_uppercase())
        {
            add_windows_env_key_value_to_block(
                &mut converted_block,
                custom_key_os,
                OsStr::new(custom_value),
            );
        } else {
            warn!(
                "Omitting environment variable pair with \
                 duplicate key: '{custom_key}={custom_value}'"
            );
        }
    }

    for (inherited_key, inherited_value) in std::env::vars_os() {
        if all_env_keys
            .insert(inherited_key.to_ascii_uppercase())
        {
            add_windows_env_key_value_to_block(
                &mut converted_block,
                &inherited_key,
                &inherited_value,
            );
        }
    }

    converted_block.push(0);
    Some(converted_block)
}

fn add_windows_env_key_value_to_block(
    block: &mut Vec<u16>,
    key: &OsStr,
    value: &OsStr,
) {
    block.extend(key.encode_wide());
    block.push('=' as u16);
    block.extend(value.encode_wide());
    block.push(0);
}

impl From<WindowSize> for COORD {
    fn from(window_size: WindowSize) -> Self {
        COORD {
            X: window_size.num_cols as i16,
            Y: window_size.num_lines as i16,
        }
    }
}
```

#### `crates/pty/src/windows/mod.rs`

Adapted from alacritty. `EventedReadWrite`/`EventedPty` stripped; replaced with direct methods.

```rust
use std::ffi::OsStr;
use std::io::Result;
use std::iter::once;
use std::os::windows::ffi::OsStrExt;
use std::sync::mpsc::TryRecvError;

use miow::pipe::{AnonRead, AnonWrite};

use crate::{ChildEvent, Options, Shell, WindowSize};

mod blocking;
pub(crate) mod child;
mod conpty;

use blocking::{UnblockedReader, UnblockedWriter};
use conpty::Conpty;

pub const PTY_CHILD_EVENT_TOKEN: usize = 1;
pub const PTY_READ_WRITE_TOKEN: usize = 2;

type ReadPipe = UnblockedReader<AnonRead>;
type WritePipe = UnblockedWriter<AnonWrite>;

pub struct Pty {
    // Backend MUST be the first field for correct drop order.
    // Dropping conout before backend will deadlock ClosePseudoConsole.
    backend: Conpty,
    conout: ReadPipe,
    conin: WritePipe,
    child_watcher: child::ChildExitWatcher,
}

impl Pty {
    pub(crate) fn new(
        backend: Conpty,
        conout: ReadPipe,
        conin: WritePipe,
        child_watcher: child::ChildExitWatcher,
    ) -> Self {
        Self { backend, conout, conin, child_watcher }
    }

    pub fn reader(&mut self) -> &mut ReadPipe {
        &mut self.conout
    }

    pub fn writer(&mut self) -> &mut WritePipe {
        &mut self.conin
    }

    pub fn resize(&mut self, window_size: WindowSize) {
        self.backend.resize(window_size);
    }

    pub fn next_child_event(&mut self) -> Option<ChildEvent> {
        match self.child_watcher.event_rx().try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                Some(ChildEvent::Exited(None))
            }
        }
    }

    pub fn child_watcher(
        &self,
    ) -> &child::ChildExitWatcher {
        &self.child_watcher
    }
}

pub fn new(
    config: &Options,
    window_size: WindowSize,
) -> Result<Pty> {
    conpty::new(config, window_size)
}

// --- Command-line helpers ---

fn push_escaped_arg(cmd: &mut String, arg: &str) {
    let arg_bytes = arg.as_bytes();
    let quote = arg_bytes.iter().any(|c| *c == b' ' || *c == b'\t')
        || arg_bytes.is_empty();
    if quote {
        cmd.push('"');
    }

    let mut backslashes: usize = 0;
    for x in arg.chars() {
        if x == '\\' {
            backslashes += 1;
        } else {
            if x == '"' {
                cmd.extend((0..=backslashes).map(|_| '\\'));
            }
            backslashes = 0;
        }
        cmd.push(x);
    }

    if quote {
        cmd.extend((0..backslashes).map(|_| '\\'));
        cmd.push('"');
    }
}

pub(crate) fn cmdline(config: &Options) -> String {
    let default_shell =
        Shell::new("powershell".to_owned(), Vec::new());
    let shell = config.shell.as_ref().unwrap_or(&default_shell);

    let mut cmd = String::new();
    cmd.push_str(&shell.program);

    for arg in &shell.args {
        cmd.push(' ');
        if config.escape_args {
            push_escaped_arg(&mut cmd, arg);
        } else {
            cmd.push_str(arg)
        }
    }
    cmd
}

pub fn win32_string<S: AsRef<OsStr> + ?Sized>(
    value: &S,
) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(once(0)).collect()
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_escape() {
        let test_set = vec![
            ("abc", "abc"),
            ("", "\"\""),
            (" ", "\" \""),
            ("ab c", "\"ab c\""),
            ("ab\tc", "\"ab\tc\""),
            ("ab\\c", "ab\\c"),
            ("ab\"c", "ab\\\"c"),
            ("\"", "\\\""),
            ("a\"b\"c", "a\\\"b\\\"c"),
            ("ab \"c", "\"ab \\\"c\""),
            ("a \"b\" c", "\"a \\\"b\\\" c\""),
            (
                "C:\\Program Files\\",
                "\"C:\\Program Files\\\\\"",
            ),
            (
                "C:\\Program Files\\a.txt",
                "\"C:\\Program Files\\a.txt\"",
            ),
        ];

        for (input, expected) in test_set {
            let mut escaped_arg = String::new();
            push_escaped_arg(&mut escaped_arg, input);
            assert_eq!(
                escaped_arg, expected,
                "Failed for input: {}",
                input
            );
        }
    }

    #[test]
    fn test_cmdline() {
        let mut options = Options {
            shell: Some(Shell {
                program: "echo".to_string(),
                args: vec!["hello world".to_string()],
            }),
            working_directory: None,
            env: Default::default(),
            escape_args: false,
        };
        assert_eq!(cmdline(&options), "echo hello world");

        options.escape_args = true;
        assert_eq!(cmdline(&options), "echo \"hello world\"");
    }
}
```

**Verify:** `cargo check -p pty` + `cargo test -p pty` (escape + cmdline tests pass)

---

### Task 3: Unix Backend

**File:** `crates/pty/src/unix.rs`

Adapted from alacritty's `unix.rs`. Changes:

- Imports point to `crate::*` instead of `crate::event`/`crate::tty`
- `EventedReadWrite`/`EventedPty` trait impls stripped
- `OnResize` replaced with direct `resize()` method
- Alacritty-specific env vars removed
- `die!` macro replaced with `log::error!` + error return

```rust
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{Error, ErrorKind, Read, Result};
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
#[cfg(target_os = "macos")]
use std::path::Path;
use std::process::{Child, Command};
use std::{env, ptr};

use libc::{
    F_GETFL, F_SETFL, O_NONBLOCK, TIOCSCTTY, c_int, fcntl,
};
use log::error;
use rustix_openpty::openpty;
use rustix_openpty::rustix::termios::Winsize;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use rustix_openpty::rustix::termios::{
    self, InputModes, OptionalActions,
};
use signal_hook::low_level::{
    pipe as signal_pipe, unregister as unregister_signal,
};
use signal_hook::{SigId, consts as sigconsts};

use crate::{ChildEvent, Options, WindowSize};

fn set_controlling_terminal(fd: c_int) -> Result<()> {
    let res = unsafe {
        #[allow(clippy::cast_lossless)]
        libc::ioctl(fd, TIOCSCTTY as _, 0)
    };
    if res == 0 {
        Ok(())
    } else {
        Err(Error::last_os_error())
    }
}

#[derive(Debug)]
struct Passwd<'a> {
    name: &'a str,
    dir: &'a str,
    shell: &'a str,
}

fn get_pw_entry(buf: &mut [i8; 1024]) -> Result<Passwd<'_>> {
    let mut entry: MaybeUninit<libc::passwd> =
        MaybeUninit::uninit();
    let mut res: *mut libc::passwd = ptr::null_mut();

    let uid = unsafe { libc::getuid() };
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            entry.as_mut_ptr(),
            buf.as_mut_ptr() as *mut _,
            buf.len(),
            &mut res,
        )
    };
    let entry = unsafe { entry.assume_init() };

    if status < 0 {
        return Err(Error::other("getpwuid_r failed"));
    }
    if res.is_null() {
        return Err(Error::other("pw not found"));
    }
    assert_eq!(entry.pw_uid, uid);

    Ok(Passwd {
        name: unsafe {
            CStr::from_ptr(entry.pw_name).to_str().unwrap()
        },
        dir: unsafe {
            CStr::from_ptr(entry.pw_dir).to_str().unwrap()
        },
        shell: unsafe {
            CStr::from_ptr(entry.pw_shell).to_str().unwrap()
        },
    })
}

pub struct Pty {
    child: Child,
    file: File,
    signals: UnixStream,
    sig_id: SigId,
}

impl Pty {
    pub fn child(&self) -> &Child {
        &self.child
    }

    pub fn reader(&mut self) -> &mut File {
        &mut self.file
    }

    pub fn writer(&mut self) -> &mut File {
        &mut self.file
    }

    pub fn resize(&mut self, window_size: WindowSize) {
        let win = window_size.to_winsize();
        let res = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                libc::TIOCSWINSZ,
                &win as *const _,
            )
        };
        if res < 0 {
            error!(
                "ioctl TIOCSWINSZ failed: {}",
                Error::last_os_error()
            );
        }
    }

    pub fn next_child_event(&mut self) -> Option<ChildEvent> {
        let mut buf = [0u8; 1];
        if let Err(err) = self.signals.read(&mut buf) {
            if err.kind() != ErrorKind::WouldBlock {
                error!(
                    "Error reading from signal pipe: {err}"
                );
            }
            return None;
        }

        match self.child.try_wait() {
            Err(err) => {
                error!(
                    "Error checking child process \
                     termination: {err}"
                );
                None
            }
            Ok(None) => None,
            Ok(exit_status) => {
                Some(ChildEvent::Exited(exit_status))
            }
        }
    }
}

struct ShellUser {
    user: String,
    home: String,
    shell: String,
}

impl ShellUser {
    fn from_env() -> Result<Self> {
        let mut buf = [0; 1024];
        let pw = get_pw_entry(&mut buf);

        let user = match env::var("USER") {
            Ok(user) => user,
            Err(_) => match pw {
                Ok(ref pw) => pw.name.to_owned(),
                Err(err) => return Err(err),
            },
        };

        let home = match env::var("HOME") {
            Ok(home) => home,
            Err(_) => match pw {
                Ok(ref pw) => pw.dir.to_owned(),
                Err(err) => return Err(err),
            },
        };

        let shell = match env::var("SHELL") {
            Ok(shell) => shell,
            Err(_) => match pw {
                Ok(ref pw) => pw.shell.to_owned(),
                Err(err) => return Err(err),
            },
        };

        Ok(Self { user, home, shell })
    }
}

#[cfg(not(target_os = "macos"))]
fn default_shell_command(
    shell: &str,
    _user: &str,
    _home: &str,
) -> Command {
    Command::new(shell)
}

#[cfg(target_os = "macos")]
fn default_shell_command(
    shell: &str,
    user: &str,
    home: &str,
) -> Command {
    let shell_name = shell.rsplit('/').next().unwrap();
    let mut login_command = Command::new("/usr/bin/login");
    let exec =
        format!("exec -a -{} {}", shell_name, shell);
    let has_home_hushlogin =
        Path::new(home).join(".hushlogin").exists();
    let flags = if has_home_hushlogin {
        "-qflp"
    } else {
        "-flp"
    };
    login_command
        .args([flags, user, "/bin/zsh", "-fc", &exec]);
    login_command
}

pub fn new(
    config: &Options,
    window_size: WindowSize,
) -> Result<Pty> {
    let pty = openpty(None, Some(&window_size.to_winsize()))?;
    let (master, slave) = (pty.controller, pty.user);
    from_fd(config, master, slave)
}

fn from_fd(
    config: &Options,
    master: OwnedFd,
    slave: OwnedFd,
) -> Result<Pty> {
    let master_fd = master.as_raw_fd();
    let slave_fd = slave.as_raw_fd();

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Ok(mut termios) = termios::tcgetattr(&master) {
        termios.input_modes.set(InputModes::IUTF8, true);
        let _ = termios::tcsetattr(
            &master,
            OptionalActions::Now,
            &termios,
        );
    }

    let user = ShellUser::from_env()?;

    let mut builder = if let Some(shell) = config.shell.as_ref()
    {
        let mut cmd = Command::new(&shell.program);
        cmd.args(shell.args.as_slice());
        cmd
    } else {
        default_shell_command(
            &user.shell, &user.user, &user.home,
        )
    };

    builder.stdin(slave.try_clone()?);
    builder.stderr(slave.try_clone()?);
    builder.stdout(slave);

    builder.env("USER", user.user);
    builder.env("HOME", user.home);
    for (key, value) in &config.env {
        builder.env(key, value);
    }

    builder.env_remove("XDG_ACTIVATION_TOKEN");
    builder.env_remove("DESKTOP_STARTUP_ID");

    let working_directory = config
        .working_directory
        .as_ref()
        .and_then(|path| {
            CString::new(path.as_os_str().as_bytes()).ok()
        });

    unsafe {
        builder.pre_exec(move || {
            let err = libc::setsid();
            if err == -1 {
                return Err(Error::last_os_error());
            }

            if let Some(working_directory) =
                working_directory.as_ref()
            {
                libc::chdir(working_directory.as_ptr());
            }

            set_controlling_terminal(slave_fd)?;

            libc::close(slave_fd);
            libc::close(master_fd);

            libc::signal(libc::SIGCHLD, libc::SIG_DFL);
            libc::signal(libc::SIGHUP, libc::SIG_DFL);
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::signal(libc::SIGQUIT, libc::SIG_DFL);
            libc::signal(libc::SIGTERM, libc::SIG_DFL);
            libc::signal(libc::SIGALRM, libc::SIG_DFL);

            Ok(())
        });
    }

    let (signals, sig_id) = {
        let (sender, recv) = UnixStream::pair()?;
        let sig_id =
            signal_pipe::register(sigconsts::SIGCHLD, sender)?;
        recv.set_nonblocking(true)?;
        (recv, sig_id)
    };

    match builder.spawn() {
        Ok(child) => {
            unsafe {
                set_nonblocking(master_fd);
            }
            Ok(Pty {
                child,
                file: File::from(master),
                signals,
                sig_id,
            })
        }
        Err(err) => Err(Error::new(
            err.kind(),
            format!(
                "Failed to spawn command '{}': {}",
                builder.get_program().to_string_lossy(),
                err
            ),
        )),
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGHUP);
        }
        unregister_signal(self.sig_id);
        let _ = self.child.wait();
    }
}

trait ToWinsize {
    fn to_winsize(self) -> Winsize;
}

impl ToWinsize for WindowSize {
    fn to_winsize(self) -> Winsize {
        let ws_row = self.num_lines as libc::c_ushort;
        let ws_col = self.num_cols as libc::c_ushort;
        let ws_xpixel =
            ws_col * self.cell_width as libc::c_ushort;
        let ws_ypixel =
            ws_row * self.cell_height as libc::c_ushort;
        Winsize { ws_row, ws_col, ws_xpixel, ws_ypixel }
    }
}

unsafe fn set_nonblocking(fd: c_int) {
    let res = unsafe {
        fcntl(fd, F_SETFL, fcntl(fd, F_GETFL, 0) | O_NONBLOCK)
    };
    assert_eq!(res, 0);
}

#[test]
fn test_get_pw_entry() {
    let mut buf: [i8; 1024] = [0; 1024];
    let _pw = get_pw_entry(&mut buf).unwrap();
}
```

**Verify:** `cargo check -p pty` (compiles to nothing on Windows due to `#[cfg(unix)]`).

---

### Task 4: Poll-Driven Channel Adapter (`handle.rs`)

**File:** `crates/pty/src/handle.rs`

This is the new code — the bridge between the platform `Pty` and the
`PtyCommand`/`PtyEvent` channel protocol from `docs/architecture/04-pty-threading.md`.

**Design constraints** (from oracle review):

1. Alacritty's Windows `UnblockedReader::read()` returns `Ok(0)` when empty
   (not EOF). A naive blocking loop will busy-spin. Must use `polling`.
2. `UnblockedWriter::write()` returns `Ok(0)` when pipe is full. Must buffer
   writes and drain on writable events.
3. Child exit must be detected via `next_child_event()`, not EOF on read.
   ConPTY doesn't reliably signal EOF.
4. Shutdown must drain conout before dropping the ConPTY backend to avoid
   `ClosePseudoConsole` deadlock.
5. During shutdown, `event_tx` must not block (switch to `try_send`).

```rust
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use polling::{Event, Events, PollMode, Poller};

use crate::{
    Options, Pty, PtyCommand, PtyEvent,
    PTY_COMMAND_CHANNEL_CAPACITY, PTY_EVENT_CHANNEL_CAPACITY,
    WindowSize,
};

#[cfg(windows)]
use crate::windows::{PTY_CHILD_EVENT_TOKEN, PTY_READ_WRITE_TOKEN};

const READ_BUF_SIZE: usize = 0x10_0000; // 1MB
const SHUTDOWN_DRAIN_TIMEOUT: Duration =
    Duration::from_millis(500);

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
    pub fn spawn(
        options: Options,
        window_size: WindowSize,
    ) -> std::io::Result<Self> {
        let (event_tx, event_rx) =
            mpsc::sync_channel(PTY_EVENT_CHANNEL_CAPACITY);
        let (command_tx, command_rx) =
            mpsc::sync_channel(PTY_COMMAND_CHANNEL_CAPACITY);

        // Spawn the platform PTY before moving to worker thread
        // so we can return spawn errors synchronously.
        let pty = crate::new(&options, window_size)?;

        let worker = thread::Builder::new()
            .name("pty-worker".into())
            .spawn(move || {
                worker_loop(pty, event_tx, command_rx);
            })
            .map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("failed to spawn pty worker: {e}"),
                )
            })?;

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

fn worker_loop(
    mut pty: Pty,
    event_tx: SyncSender<PtyEvent>,
    command_rx: Receiver<PtyCommand>,
) {
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
        pty.reader().register(
            &poller,
            interest,
            PollMode::Level,
        );
        pty.writer().register(
            &poller,
            interest,
            PollMode::Level,
        );
        pty.child_watcher().register(
            &poller,
            Event::readable(PTY_CHILD_EVENT_TOKEN),
        );
    }

    #[cfg(unix)]
    unsafe {
        let interest = Event::all(PTY_READ_WRITE_TOKEN);
        if let Err(e) = poller.add_with_mode(
            pty.reader(),
            interest,
            PollMode::Level,
        ) {
            event_tx.send(PtyEvent::Error(e)).ok();
            return;
        }
    }

    let mut events = Events::new();
    let mut read_buf = vec![0u8; READ_BUF_SIZE];
    let mut write_buf: VecDeque<u8> = VecDeque::new();
    let mut closing = false;
    let mut child_exited = false;

    loop {
        events.clear();

        let timeout = if closing {
            Some(SHUTDOWN_DRAIN_TIMEOUT)
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
                            Err(e)
                                if e.kind()
                                    == std::io::ErrorKind::WouldBlock =>
                            {
                                0
                            }
                            Err(e) => {
                                event_tx
                                    .send(PtyEvent::Error(e))
                                    .ok();
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
                    event_tx
                        .try_send(PtyEvent::Output(data))
                        .ok();
                } else {
                    if event_tx
                        .send(PtyEvent::Output(data))
                        .is_err()
                    {
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
                            Err(e)
                                if e.kind()
                                    == std::io::ErrorKind::WouldBlock =>
                            {
                                0
                            }
                            Err(e) => {
                                log::error!(
                                    "PTY write error: {e}"
                                );
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

        if child_event || !child_exited {
            if let Some(child_event) =
                pty.next_child_event()
            {
                child_exited = true;
                if closing {
                    event_tx
                        .try_send(PtyEvent::Exited(
                            match child_event {
                                crate::ChildEvent::Exited(s) => {
                                    s
                                }
                            },
                        ))
                        .ok();
                } else {
                    event_tx
                        .send(PtyEvent::Exited(
                            match child_event {
                                crate::ChildEvent::Exited(s) => {
                                    s
                                }
                            },
                        ))
                        .ok();
                }
                // Child exited — begin shutdown.
                closing = true;
            }
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
                    closing = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
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

        if closing && !child_exited {
            // We're closing but child hasn't exited yet.
            // The timeout on the poll loop will bound this.
            // After SHUTDOWN_DRAIN_TIMEOUT we'll break out.
        }

        // Re-register for polling on Unix (level-triggered
        // should be automatic, but reregister for edge cases).
        #[cfg(unix)]
        {
            let interest = Event::all(PTY_READ_WRITE_TOKEN);
            let _ = poller.modify_with_mode(
                pty.reader(),
                interest,
                PollMode::Level,
            );
        }

        #[cfg(windows)]
        {
            let interest = Event::all(PTY_READ_WRITE_TOKEN);
            pty.reader().register(
                &poller,
                interest,
                PollMode::Level,
            );
            pty.writer().register(
                &poller,
                interest,
                PollMode::Level,
            );
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
        event_tx
            .try_send(PtyEvent::Exited(None))
            .ok();
    }
}

// Platform new() re-export for handle.rs to call.
#[cfg(windows)]
fn _new_pty(
    config: &Options,
    window_size: WindowSize,
) -> std::io::Result<Pty> {
    crate::windows::new(config, window_size)
}

#[cfg(unix)]
fn _new_pty(
    config: &Options,
    window_size: WindowSize,
) -> std::io::Result<Pty> {
    crate::unix::new(config, window_size)
}
```

Also add a `new()` function to `lib.rs` that dispatches to the platform:

Add to the bottom of `crates/pty/src/lib.rs`:

```rust
/// Create a new platform PTY.
pub fn new(
    config: &Options,
    window_size: WindowSize,
) -> std::io::Result<Pty> {
    #[cfg(windows)]
    {
        windows::new(config, window_size)
    }
    #[cfg(unix)]
    {
        unix::new(config, window_size)
    }
}
```

**Verify:** `cargo check -p pty`

---

### Task 5: Integration Tests

**File:** `crates/pty/tests/integration.rs` (or inline in `handle.rs`)

```rust
#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::{
        Options, PtyCommand, PtyEvent, PtyHandle, Shell,
        WindowSize,
    };

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
        let handle = PtyHandle::spawn(
            test_options(),
            test_window_size(),
        )
        .unwrap();

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
        let handle = PtyHandle::spawn(
            test_options(),
            test_window_size(),
        )
        .unwrap();

        // Wait for shell prompt.
        std::thread::sleep(Duration::from_millis(500));

        // Drain any initial output.
        while handle
            .event_rx
            .try_recv()
            .is_ok()
        {}

        #[cfg(windows)]
        let cmd = b"echo hello\r\n".to_vec();
        #[cfg(unix)]
        let cmd = b"echo hello\n".to_vec();

        handle
            .command_tx
            .send(PtyCommand::Write(cmd))
            .unwrap();

        let mut output = String::new();
        let deadline =
            std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match handle
                .event_rx
                .recv_timeout(Duration::from_millis(100))
            {
                Ok(PtyEvent::Output(data)) => {
                    output.push_str(
                        &String::from_utf8_lossy(&data),
                    );
                    if output.contains("hello") {
                        return; // pass
                    }
                }
                Ok(PtyEvent::Exited(_)) => break,
                _ => continue,
            }
        }
        panic!(
            "did not find 'hello' in output. got: {:?}",
            output
        );
    }

    #[test]
    fn resize_does_not_crash() {
        let handle = PtyHandle::spawn(
            test_options(),
            test_window_size(),
        )
        .unwrap();

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

        handle
            .command_tx
            .send(PtyCommand::Close)
            .unwrap();
    }

    #[test]
    fn close_emits_exited() {
        let handle = PtyHandle::spawn(
            test_options(),
            test_window_size(),
        )
        .unwrap();

        handle
            .command_tx
            .send(PtyCommand::Close)
            .unwrap();

        let deadline =
            std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match handle
                .event_rx
                .recv_timeout(Duration::from_millis(100))
            {
                Ok(PtyEvent::Exited(_)) => return,
                Ok(_) => continue,
                Err(_) => continue,
            }
        }
        panic!("did not receive Exited event");
    }
}
```

Add these as `#[cfg(test)] mod tests { ... }` at the bottom of `handle.rs`.

**Verify:** `cargo test -p pty`

---

### Task 6: Update Architecture Docs + dot Issue

**Files:**

- Modify: `docs/architecture/01-system-overview.md` — replace `portable-pty` references:
  - Line 9: `- \`portable-pty\` for PTY/process integration`→`- Alacritty-derived PTY backend (ConPTY on Windows, openpty on Unix)`
  - Line 25: `pty/           -> portable-pty wrapper + process/session lifecycle` → `pty/           -> ConPTY/openpty backend + PtyHandle channel adapter`
- Modify: `docs/architecture/04-pty-threading.md` — update "v0 Backend" section:
  - Replace lines 9-10 with:
    ```
    - Alacritty-derived ConPTY backend on Windows (supports conpty.dll from Windows Terminal)
    - Alacritty-derived openpty backend on Unix (macOS/Linux)
    - Default shell: `powershell.exe` (Windows), `$SHELL` (Unix)
    ```
- Update dot issue: `dot edit pty-001` — update description to reflect alacritty backend
