use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::Result;
use async_channel::Sender as AsyncSender;
use crossbeam_channel::{Receiver, TryRecvError, select};
use font::backend::dwrite::analyzer::DWriteAnalyzer;
use font::backend::dwrite::fallback::FontFallbackContext;
use font::cache::shaped_run_cache::ShapedRunCache;
use font::shaper::Shaper;
use font::shared_grid_set::{DWriteGridConfig, DWriteGridKey, SharedGridPtr, SharedGridSet};
use ghostty::{RenderFrame, ScrollbarInfo, Terminal};
use gpui::CompositionSlotEvent;
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
    CellMetrics, Contents, DWriteGlyphRasterizer, build_batch, build_shape_options,
    grid_metrics_from_renderer_config, measure_renderer_cell_metrics, ui_metrics,
};
use super::shared_grid_ptr::shared_grid_ref;
use super::terminal_renderer::{RendererTextConfig, RendererUiUpdate};
use super::types::RenderBatch;

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
    terminal: Arc<Mutex<Terminal>>,
    text_config: RendererTextConfig,
    ui_tx: AsyncSender<RendererUiUpdate>,
    slot_rx: Receiver<CompositionSlotEvent>,
) -> RendererThreadHandle {
    let (tx, rx) = crossbeam_channel::bounded::<RendererMessage>(64);

    let join = std::thread::Builder::new()
        .name("terminal-renderer".into())
        .spawn(move || {
            let mut thread = RendererThread::new(device, swap_chain, terminal, text_config, ui_tx)
                .expect("failed to initialize renderer thread");
            thread.run(rx, slot_rx);
        })
        .expect("failed to spawn renderer thread");

    RendererThreadHandle {
        tx,
        join: Some(join),
    }
}

struct RendererThread {
    backend: D3D11Backend,
    terminal: Arc<Mutex<Terminal>>,
    config: RendererTextConfig,
    shared_grid: SharedGridPtr,
    shaper: Shaper,
    shaper_cache: ShapedRunCache,
    contents: Contents,
    cell_metrics: CellMetrics,
    rasterizer: DWriteGlyphRasterizer,
    batch: RenderBatch,
    ui_tx: AsyncSender<RendererUiUpdate>,
    last_scrollbar: Option<ScrollbarInfo>,
    grid_set: &'static SharedGridSet<DWriteGridKey>,
    grid_key: DWriteGridKey,
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
        terminal: Arc<Mutex<Terminal>>,
        text_config: RendererTextConfig,
        ui_tx: AsyncSender<RendererUiUpdate>,
    ) -> Result<Self> {
        let dwrite_factory2: IDWriteFactory2 =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let dwrite_factory6 = dwrite_factory2.cast::<IDWriteFactory6>()?;
        let mut analyzer = DWriteAnalyzer::new(&dwrite_factory2)?;
        analyzer.reserve_for(8192, 1024)?;

        let fallback = build_fallback_context(&dwrite_factory6)?;
        let mut grid_config =
            DWriteGridConfig::with_single_family(text_config.font_family.as_ref(), "en-US");
        grid_config.metrics = grid_metrics_from_renderer_config(&text_config);
        grid_config.max_atlas_size = D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION;
        grid_config.fallback = fallback;

        // Ghostty reference: `SharedGridSet.ref` key-based lifecycle ownership.
        let grid_set = dwrite_shared_grid_set();
        let (grid_key, shared_grid) = grid_set.ref_dwrite(&dwrite_factory6, &grid_config)?;

        let locale = "en-US";
        let feature_spec = font::types::FontFeatureSpec::default();
        let cell_metrics = measure_renderer_cell_metrics(
            shared_grid_ref(shared_grid),
            &dwrite_factory2,
            &text_config,
        );
        shared_grid_ref(shared_grid).set_metrics(font::shared_grid::GridMetrics {
            cell_width: cell_metrics.cell_width.round().max(1.0) as u16,
            cell_height: cell_metrics.line_height.round().max(1.0) as u16,
        });
        let shape_options = build_shape_options(&text_config, &cell_metrics, locale, &feature_spec);
        let shaper = Shaper::new(analyzer, shape_options);
        let metrics = ui_metrics(cell_metrics);
        ui_tx.try_send(RendererUiUpdate::Metrics(metrics)).ok();

        Ok(Self {
            backend: D3D11Backend::new(device, swap_chain, shared_grid)?,
            terminal,
            config: text_config,
            shared_grid,
            shaper,
            shaper_cache: ShapedRunCache::new(),
            contents: Contents::new(),
            cell_metrics,
            rasterizer: DWriteGlyphRasterizer::new(dwrite_factory2),
            batch: RenderBatch::default(),
            ui_tx,
            last_scrollbar: None,
            grid_set,
            grid_key,
        })
    }

    fn run(&mut self, rx: Receiver<RendererMessage>, slot_rx: Receiver<CompositionSlotEvent>) {
        let mut wait_for_presentation = false;

        loop {
            if wait_for_presentation {
                self.backend.wait_for_frame_latency();
                wait_for_presentation = false;
            }

            let mut pending_wake = false;
            let mut first_slot_event = None;

            select! {
                recv(slot_rx) -> msg => {
                    match msg {
                        Ok(event) => first_slot_event = Some(event),
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

            if first_slot_event.is_none() {
                first_slot_event = match slot_rx.try_recv() {
                    Ok(event) => Some(event),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => return,
                };
            }

            let slot_needs_present = if let Some(first_event) = first_slot_event {
                match self.backend.process_slot_event_batch(first_event, &slot_rx) {
                    Ok(value) => value,
                    Err(err) => {
                        log::error!("slot event handling failed: {err:#}");
                        false
                    }
                }
            } else {
                false
            };

            let should_present = (pending_wake || slot_needs_present) && self.backend.has_target();
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
        let (frame, scrollbar) = {
            let mut terminal = self.terminal.lock().expect("terminal mutex poisoned");
            (terminal.render_frame(), terminal.scrollbar_info())
        };

        if scrollbar_changed(self.last_scrollbar, scrollbar) {
            self.last_scrollbar = Some(scrollbar);
            self.ui_tx
                .try_send(RendererUiUpdate::Scrollbar(scrollbar))
                .ok();
        }

        self.build_batch(&frame)?;
        self.backend.draw_and_present(
            &self.batch,
            self.contents.fg_lists(),
            &self.contents.bg_cells,
        )
    }

    fn build_batch(&mut self, frame: &RenderFrame) -> Result<()> {
        build_batch(
            &self.config,
            self.shared_grid,
            &mut self.shaper,
            &mut self.shaper_cache,
            &mut self.contents,
            &self.cell_metrics,
            &self.rasterizer,
            frame,
            &mut self.batch,
        )
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
