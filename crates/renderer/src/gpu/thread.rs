//! The main renderer loop
//! TODO: Doc comments
use crossbeam_channel::{Receiver, TryRecvError, after, never, select};
use gpui::ExternalSurfaceEvent;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain2;

use crate::gpu::renderer::Renderer;
use crate::types::{DerivedConfig, FrameOutcome};
use anyhow::Result;
use crossbeam_channel::Sender;
use font::shared_grid::SharedGrid;
use ghostty::{RenderState, ScrollbarInfo, Terminal};

const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(600);

#[derive(Clone)]
pub enum RendererUiUpdate {
    Scrollbar(ScrollbarInfo),
}

/// Messages sent from the terminal/IO path to the renderer thread.
// TODO: Courier
pub enum RendererMessage {
    /// Set a new grid as the shared grid.
    /// Sent on font size changes.
    SetFontGrid(Arc<SharedGrid>),
    /// Update whether the terminal view has keyboard focus.
    SetFocus(bool),
    /// Ordered shutdown; renderer thread should exit its loop.
    Quit,
}

pub struct RendererThread {
    renderer: Renderer,
    render_state: RenderState,
    cursor_blink_visible: bool,
    cursor_blink_deadline: Option<Instant>,
}

pub struct ThreadOptions<'a> {
    pub swap_chain: &'a mut Option<IDXGISwapChain2>,
    pub focused: bool,
}

impl RendererThread {
    pub fn new(
        config: DerivedConfig,
        grid: Arc<SharedGrid>,
        terminal: Arc<Terminal>,
        ui_tx: async_channel::Sender<RendererUiUpdate>,
        surface_rx: Receiver<ExternalSurfaceEvent>,
        options: ThreadOptions,
    ) -> Result<RendererThreadHandle> {
        let (wake_tx, wake_rx) = crossbeam_channel::bounded::<()>(1);
        // Control traffic is low-volume and must not be dropped: losing a
        // focus transition would leave the cursor in the wrong visual state.
        let (control_tx, control_rx) = crossbeam_channel::unbounded::<RendererMessage>();

        let (tx, rx) = crossbeam_channel::bounded(1);

        let handle = std::thread::Builder::new()
            .name("terminal-renderer".into())
            .spawn(move || {
                let renderer = Renderer::new(config, terminal, grid, ui_tx, options.focused)
                    .expect("Failed to start terminal renderer");
                let render_state =
                    RenderState::new().expect("Failed to allocate terminal render state");
                tx.send(renderer.swap_chain().clone()).unwrap();

                let mut thread = RendererThread {
                    renderer,
                    render_state,
                    cursor_blink_visible: true,
                    cursor_blink_deadline: None,
                };
                thread.run(wake_rx, control_rx, surface_rx);
            })?;

        *options.swap_chain = Some(rx.recv().unwrap());

        Ok(RendererThreadHandle {
            wake_tx,
            control_tx,
            join_handle: Some(handle),
        })
    }

    /// Returns `false` when the renderer should stop.
    fn handle_control(&mut self, message: RendererMessage, pending_wake: &mut bool) -> bool {
        match message {
            RendererMessage::SetFontGrid(grid) => {
                if let Err(error) = self.renderer.set_font_grid(grid) {
                    log::error!("failed to update renderer font grid: {error:#}");
                }
            }
            RendererMessage::SetFocus(focused) => {
                self.renderer.set_focused(focused);
                match focused {
                    true => self.reset_cursor_blink(),
                    false => self.cursor_blink_deadline = None,
                }
            }
            RendererMessage::Quit => return false,
        }

        *pending_wake = true;
        true
    }

    #[inline]
    fn reset_cursor_blink(&mut self) {
        self.cursor_blink_visible = true;
        self.cursor_blink_deadline = Some(Instant::now() + CURSOR_BLINK_INTERVAL);
    }

    pub fn run(
        &mut self,
        wake_rx: Receiver<()>,
        control_rx: Receiver<RendererMessage>,
        surface_rx: Receiver<ExternalSurfaceEvent>,
    ) {
        let mut wait_for_presentation = true;
        loop {
            if wait_for_presentation {
                self.renderer.wait_for_frame();
                wait_for_presentation = false;
            }

            let mut pending_wake = false;
            let mut latest_surface_state = None;
            // TODO: after() allocates an Arc-backed timer channel each loop iteration
            let cursor_timer: Receiver<Instant> = match self.cursor_blink_deadline {
                Some(deadline) => after(deadline.saturating_duration_since(Instant::now())),
                None => never(),
            };

            select! {
                recv(wake_rx) -> wake => match wake {
                    Ok(()) => {
                        self.reset_cursor_blink();
                        pending_wake = true;
                    }
                    Err(_) => return,
                },
                recv(surface_rx) -> event => match event {
                    Ok(ExternalSurfaceEvent::StateChanged(state)) => {
                        latest_surface_state = Some(state);
                    }
                    Ok(ExternalSurfaceEvent::Dropped) | Err(_) => return,
                },
                recv(control_rx) -> msg => match msg {
                    Ok(message) => if !self.handle_control(message, &mut pending_wake) {
                        return;
                    },
                    Err(_) => return,
                },
                recv(cursor_timer) -> _ => {
                    if self.renderer.cursor_blink_active() {
                        self.cursor_blink_visible = !self.cursor_blink_visible;
                        self.cursor_blink_deadline =
                            Some(Instant::now() + CURSOR_BLINK_INTERVAL);
                        pending_wake = true;
                    } else {
                        self.cursor_blink_visible = true;
                        self.cursor_blink_deadline = None;
                    }
                }
            }

            while let Ok(_) = wake_rx.try_recv() {
                self.reset_cursor_blink();
                pending_wake = true
            }

            while let Ok(message) = control_rx.try_recv() {
                if !self.handle_control(message, &mut pending_wake) {
                    return;
                }
            }

            while let Ok(event) = surface_rx.try_recv() {
                match event {
                    ExternalSurfaceEvent::StateChanged(state) => {
                        latest_surface_state = Some(state);
                    }
                    ExternalSurfaceEvent::Dropped => return,
                }
            }

            let needs_redraw = match latest_surface_state {
                Some(state) => match self.renderer.apply_surface_state(state) {
                    Ok(changed) => changed,
                    Err(e) => {
                        log::error!("external surface event handling failed: {e:#}");
                        false
                    }
                },
                None => false,
            };

            if pending_wake {
                // Hack to workaround powershell cursor flickering when typing
                const WAKE_COALESCE_WINDOW: Duration = Duration::from_millis(1);

                let deadline = Instant::now() + WAKE_COALESCE_WINDOW;

                pending_wake = 'wake: loop {
                    match wake_rx.try_recv() {
                        Ok(()) => self.reset_cursor_blink(),
                        Err(TryRecvError::Empty) => {}
                        _ => break 'wake false,
                    }
                    match control_rx.try_recv() {
                        Ok(message) => {
                            if !self.handle_control(message, &mut pending_wake) {
                                return;
                            }
                        }
                        Err(TryRecvError::Disconnected) => {
                            break 'wake false;
                        }
                        Err(TryRecvError::Empty) => {}
                    }

                    let now = Instant::now();
                    if now >= deadline {
                        break 'wake true;
                    }

                    std::thread::sleep(deadline - now);
                };
            }

            if pending_wake || needs_redraw {
                let update_result = self
                    .renderer
                    .update_frame(&mut self.render_state, self.cursor_blink_visible);
                if let Err(e) = update_result {
                    log::error!("renderer frame update failed: {e:#}");
                    continue;
                }
                if self.renderer.cursor_blink_active() && self.cursor_blink_deadline.is_none() {
                    self.reset_cursor_blink();
                }
                match self.renderer.draw_frame() {
                    Ok(FrameOutcome::Presented) => wait_for_presentation = true,
                    Ok(FrameOutcome::Skipped) => {}
                    Err(e) => log::error!("renderer present failed: {e:#}"),
                }
            }
        }
    }
}

// TODO: Courier
pub struct RendererThreadHandle {
    wake_tx: Sender<()>,
    control_tx: Sender<RendererMessage>,
    join_handle: Option<JoinHandle<()>>,
}

impl RendererThreadHandle {
    pub fn waker(&self) -> Sender<()> {
        self.wake_tx.clone()
    }

    #[inline]
    pub fn set_font_grid(&self, grid: Arc<SharedGrid>) {
        _ = self.control_tx.send(RendererMessage::SetFontGrid(grid));
    }

    #[inline]
    pub fn set_focus(&self, focused: bool) {
        _ = self.control_tx.send(RendererMessage::SetFocus(focused));
    }
}

impl Drop for RendererThreadHandle {
    fn drop(&mut self) {
        _ = self.control_tx.send(RendererMessage::Quit);
        if let Some(handle) = self.join_handle.take() {
            _ = handle.join();
        }
    }
}
