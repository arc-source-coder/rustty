use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_channel::Sender as AsyncSender;
use crossbeam_channel::{Receiver, TryRecvError, select};
use font::backend::dwrite::analyzer::DWriteAnalyzer;
use font::backend::dwrite::fallback::FontFallbackContext;
use font::cache::shaped_run_cache::ShapedRunCache;
use font::shaper::Shaper;
use font::shared_grid_set::{DWriteGridConfig, DWriteGridKey, SharedGridPtr, SharedGridSet};
use ghostty::{ScrollbarInfo, Terminal};
use gpui::{ExternalSurfaceEvent, ExternalSurfaceState};
use terminal::RendererMessage;
use windows::Win32::Graphics::Direct3D11::{D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION, ID3D11Device};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_FAMILY_MODEL_TYPOGRAPHIC, DWriteCreateFactory,
    IDWriteFactory2, IDWriteFactory6,
};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain1;
use windows::core::{HSTRING, Interface};

use super::backend_d3d11::D3D11Backend;
use super::scene::{
    CellMetrics, Contents, build_batch, build_shape_options, cell_metrics_from_grid, ui_metrics,
};
use super::shared_grid_ptr::shared_grid_ref;
use super::terminal_renderer::{RendererTextConfig, RendererUiUpdate};
use super::types::RenderBatch;

const WAKE_COALESCE_WINDOW: Duration = Duration::from_millis(1);

fn dwrite_shared_grid_set() -> &'static SharedGridSet<DWriteGridKey> {
    static GRID_SET: OnceLock<SharedGridSet<DWriteGridKey>> = OnceLock::new();
    GRID_SET.get_or_init(SharedGridSet::<DWriteGridKey>::new)
}

pub struct RendererThreadHandle {
    tx: crossbeam_channel::Sender<RendererMessage>,
    join: Option<JoinHandle<()>>,
}

impl RendererThreadHandle {
    pub fn sender(&self) -> crossbeam_channel::Sender<RendererMessage> {
        self.tx.clone()
    }
}

impl Drop for RendererThreadHandle {
    fn drop(&mut self) {
        self.tx.send(RendererMessage::Quit).ok();
        if let Some(handle) = self.join.take() {
            handle.join().ok();
        }
    }
}

pub fn spawn(
    device: ID3D11Device,
    swap_chain: IDXGISwapChain1,
    terminal: Arc<Terminal>,
    text_config: RendererTextConfig,
    ui_tx: AsyncSender<RendererUiUpdate>,
    surface_state: Arc<Mutex<ExternalSurfaceState>>,
    surface_event_rx: Receiver<ExternalSurfaceEvent>,
) -> RendererThreadHandle {
    let (tx, rx) = crossbeam_channel::bounded::<RendererMessage>(1);

    let join = std::thread::Builder::new()
        .name("terminal-renderer".into())
        .spawn(move || {
            #[cfg(feature = "profiler")]
            tracy_client::set_thread_name!("terminal-renderer");
            let mut thread = RendererThread::new(device, swap_chain, terminal, text_config, ui_tx)
                .expect("failed to initialize renderer thread");
            thread.run(rx, surface_state, surface_event_rx);
        })
        .expect("failed to spawn renderer thread");

    RendererThreadHandle {
        tx,
        join: Some(join),
    }
}

struct RendererThread {
    backend: D3D11Backend,
    terminal: Arc<Terminal>,
    config: RendererTextConfig,
    shared_grid: SharedGridPtr,
    shaper: Shaper,
    shaper_cache: ShapedRunCache,
    contents: Contents,
    cell_metrics: CellMetrics,
    batch: RenderBatch,
    ui_tx: AsyncSender<RendererUiUpdate>,
    last_scrollbar: Option<ScrollbarInfo>,
    grid_set: &'static SharedGridSet<DWriteGridKey>,
    grid_key: DWriteGridKey,
}

struct RendererTextState {
    shared_grid: SharedGridPtr,
    grid_key: DWriteGridKey,
    shaper: Shaper,
    cell_metrics: CellMetrics,
}

enum SurfaceEventOutcome {
    Continue(bool),
    Dropped,
}

impl Drop for RendererThread {
    fn drop(&mut self) {
        self.grid_set.deref(&self.grid_key);
    }
}

fn scrollbar_changed(previous: Option<ScrollbarInfo>, next: ScrollbarInfo) -> bool {
    let Some(previous) = previous else {
        return true;
    };

    previous.total_rows != next.total_rows
        || previous.top_row != next.top_row
        || previous.viewport_rows != next.viewport_rows
}

impl RendererThread {
    fn new(
        device: ID3D11Device,
        swap_chain: IDXGISwapChain1,
        terminal: Arc<Terminal>,
        text_config: RendererTextConfig,
        ui_tx: AsyncSender<RendererUiUpdate>,
    ) -> Result<Self> {
        // Ghostty reference: `SharedGridSet.ref` key-based lifecycle ownership.
        let grid_set = dwrite_shared_grid_set();

        let text_state = build_renderer_text_state(&text_config, grid_set)?;
        let metrics = ui_metrics(text_state.cell_metrics);
        ui_tx.try_send(RendererUiUpdate::Metrics(metrics)).ok();

        Ok(Self {
            backend: D3D11Backend::new(device, swap_chain, text_state.shared_grid)?,
            terminal,
            config: text_config,
            shared_grid: text_state.shared_grid,
            shaper: text_state.shaper,
            shaper_cache: ShapedRunCache::new(),
            contents: Contents::new(),
            cell_metrics: text_state.cell_metrics,
            batch: RenderBatch::default(),
            ui_tx,
            last_scrollbar: None,
            grid_set,
            grid_key: text_state.grid_key,
        })
    }

    fn run(
        &mut self,
        rx: Receiver<RendererMessage>,
        surface_state: Arc<Mutex<ExternalSurfaceState>>,
        surface_event_rx: Receiver<ExternalSurfaceEvent>,
    ) {
        let mut wait_for_presentation = false;

        loop {
            if wait_for_presentation {
                self.backend.wait_for_frame_latency();
                wait_for_presentation = false;
            }

            let mut pending_wake = false;
            let mut first_surface_event = None;

            select! {
                recv(surface_event_rx) -> msg => {
                    match msg {
                        Ok(event) => first_surface_event = Some(event),
                        Err(_) => break,
                    }
                }
                recv(rx) -> msg => {
                    match msg {
                        Ok(RendererMessage::Wake) => pending_wake = true,
                        Ok(RendererMessage::Quit) => break,
                        Err(_) => break,
                    }
                }
            }

            loop {
                match rx.try_recv() {
                    Ok(RendererMessage::Wake) => pending_wake = true,
                    Ok(RendererMessage::Quit) => return,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }

            if first_surface_event.is_none() {
                first_surface_event = match surface_event_rx.try_recv() {
                    Ok(event) => Some(event),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => return,
                };
            }

            let surface_needs_present = if let Some(first_event) = first_surface_event {
                match self.process_surface_event_batch(
                    first_event,
                    &surface_event_rx,
                    &surface_state,
                ) {
                    Ok(SurfaceEventOutcome::Continue(value)) => value,
                    Ok(SurfaceEventOutcome::Dropped) => return,
                    Err(err) => {
                        log::error!("external surface event handling failed: {err:#}");
                        false
                    }
                }
            } else {
                false
            };

            if pending_wake {
                // Hack to workaround powershell cursor flickering when typing
                pending_wake = coalesce_wakes(&rx, WAKE_COALESCE_WINDOW);
            }

            let should_present =
                (pending_wake || surface_needs_present) && self.backend.has_target();
            if !should_present {
                continue;
            }

            match self.draw_and_present() {
                Ok(()) => wait_for_presentation = true,
                Err(err) => log::error!("renderer present failed: {err:#}"),
            }
        }
    }

    fn draw_and_present(&mut self) -> Result<()> {
        let (frame, scrollbar) = unsafe {
            #[cfg(feature = "profiler")]
            let _c = tracy_client::span!("draw_and_present:contention", 32);

            // SAFETY: `render_frame()` and `scrollbar_info()` require the caller
            // to hold the Zig-owned terminal mutex. This is the one renderer
            // path that needs a coherent snapshot across both queries, so we
            // take the lock explicitly, gather both values, then unlock before
            // any heavier frame build or present work.
            self.terminal.lock();
            #[cfg(feature = "profiler")]
            let _h = tracy_client::span!("draw_and_present:hold", 32);
            let frame = self.terminal.render_frame();
            let scrollbar = self.terminal.scrollbar_info();
            self.terminal.unlock();
            (frame, scrollbar)
        };

        if scrollbar_changed(self.last_scrollbar, scrollbar) {
            self.last_scrollbar = Some(scrollbar);
            self.ui_tx
                .try_send(RendererUiUpdate::Scrollbar(scrollbar))
                .ok();
        }

        build_batch(
            &self.config,
            self.shared_grid,
            &mut self.shaper,
            &mut self.shaper_cache,
            &mut self.contents,
            &self.cell_metrics,
            &frame,
            &mut self.batch,
        )?;
        #[cfg(feature = "profiler")]
        tracy_client::frame_mark();
        self.backend.draw_and_present(
            self.shared_grid,
            &self.batch,
            self.contents.fg_lists(),
            &self.contents.bg_cells,
        )
    }

    fn process_surface_event_batch(
        &mut self,
        first: ExternalSurfaceEvent,
        surface_event_rx: &Receiver<ExternalSurfaceEvent>,
        surface_state: &Arc<Mutex<ExternalSurfaceState>>,
    ) -> Result<SurfaceEventOutcome> {
        let mut dropped = matches!(first, ExternalSurfaceEvent::Dropped);
        while let Ok(event) = surface_event_rx.try_recv() {
            if matches!(event, ExternalSurfaceEvent::Dropped) {
                dropped = true;
            }
        }
        if dropped {
            return Ok(SurfaceEventOutcome::Dropped);
        }

        let next_state = *surface_state
            .lock()
            .expect("external surface state poisoned");
        let scale_factor_changed = self.update_scale_factor(next_state.window_scale_factor)?;
        let needs_present = self.backend.apply_surface_state(next_state)? || scale_factor_changed;
        Ok(SurfaceEventOutcome::Continue(needs_present))
    }

    fn update_scale_factor(&mut self, scale_factor: f32) -> Result<bool> {
        if self.config.scale_factor == scale_factor {
            return Ok(false);
        }

        self.grid_set.deref(&self.grid_key);
        self.config.scale_factor = scale_factor;

        let text_state = build_renderer_text_state(&self.config, self.grid_set)?;
        self.shared_grid = text_state.shared_grid;
        self.grid_key = text_state.grid_key;
        self.shaper = text_state.shaper;
        self.shaper_cache = ShapedRunCache::new();
        self.contents = Contents::new();
        self.cell_metrics = text_state.cell_metrics;
        self.batch = RenderBatch::default();

        let metrics = ui_metrics(self.cell_metrics);
        self.ui_tx.try_send(RendererUiUpdate::Metrics(metrics)).ok();
        Ok(true)
    }
}

fn build_renderer_text_state(
    text_config: &RendererTextConfig,
    grid_set: &'static SharedGridSet<DWriteGridKey>,
) -> Result<RendererTextState> {
    let dwrite_factory2: IDWriteFactory2 =
        unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
    let dwrite_factory6 = dwrite_factory2.cast::<IDWriteFactory6>()?;
    let mut analyzer = DWriteAnalyzer::new(&dwrite_factory2)?;
    analyzer.reserve_for(8192, 1024)?;

    let fallback = build_fallback_context(&dwrite_factory6)?;
    let mut grid_config =
        DWriteGridConfig::with_single_family(text_config.font_family.as_ref(), "en-US");
    grid_config.font_size = text_config.font_size.as_f32();
    grid_config.raster_em_size = text_config.font_size.as_f32() * text_config.scale_factor.max(1.0);
    grid_config.cell_width = text_config.cell_width.as_f32();
    grid_config.line_height = text_config.line_height.as_f32();
    grid_config.baseline = text_config.baseline.as_f32();
    grid_config.max_atlas_size = D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION;
    grid_config.fallback = fallback;

    // Ghostty reference: `SharedGridSet.ref` key-based lifecycle ownership.
    let (grid_key, shared_grid) = grid_set.ref_dwrite(&dwrite_factory6, &grid_config)?;

    let locale = "en-US";
    // TODO(renderer-config): plumb font feature config into RendererTextConfig
    // so overlap splitting can be disabled entirely when ligatures are off.
    let feature_spec = font::types::FontFeatureSpec::default();
    let cell_metrics = cell_metrics_from_grid(
        shared_grid_ref(shared_grid).metrics(),
        text_config.font_size.as_f32(),
    );
    let shape_options = build_shape_options(text_config, &cell_metrics, locale, &feature_spec);
    let shaper = Shaper::new(analyzer, shape_options);

    Ok(RendererTextState {
        shared_grid,
        grid_key,
        shaper,
        cell_metrics,
    })
}

fn coalesce_wakes(rx: &Receiver<RendererMessage>, window: Duration) -> bool {
    let deadline = Instant::now() + window;

    loop {
        match rx.try_recv() {
            Ok(RendererMessage::Wake) => continue,
            Ok(RendererMessage::Quit) => return false,
            Err(TryRecvError::Disconnected) => return false,
            Err(TryRecvError::Empty) => {}
        }

        let now = Instant::now();
        if now >= deadline {
            return true;
        }

        std::thread::sleep(deadline - now);
    }
}

fn build_fallback_context(factory: &IDWriteFactory6) -> Result<Option<FontFallbackContext>> {
    let base_collection =
        unsafe { factory.GetSystemFontCollection(false, DWRITE_FONT_FAMILY_MODEL_TYPOGRAPHIC) }?;
    let fallback = unsafe { factory.GetSystemFontFallback() }?;
    Ok(Some(FontFallbackContext {
        base_family: HSTRING::from("Segoe UI"),
        base_collection: base_collection.cast()?,
        fallback,
    }))
}
