use anyhow::{Context, Result};
use async_channel::Sender;
use crossbeam_channel::unbounded;
use ghostty::{ScrollbarInfo, Terminal};
use gpui::{ExternalSurfaceEvent, ExternalSurfaceHost, Pixels, Window};
use std::sync::Arc;
use terminal::RendererMessage;
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    Graphics::{
        Direct3D::{
            D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
            D3D_FEATURE_LEVEL_12_0, D3D_FEATURE_LEVEL_12_1,
        },
        Direct3D11::{
            D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
        },
        DirectComposition::DCompositionCreateSurfaceHandle,
        Dxgi::{
            Common::{DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
            DXGI_SCALING_NONE, DXGI_SWAP_CHAIN_DESC1,
            DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIAdapter, IDXGIDevice, IDXGIFactory2,
            IDXGIFactoryMedia, IDXGISwapChain1,
        },
    },
};
use windows::core::Interface;

use super::thread::{RendererThreadHandle, spawn};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RendererCellMetrics {
    pub cell_width: f32,
    pub line_height: f32,
}

#[derive(Debug, Clone, Copy)]
pub enum RendererUiUpdate {
    Scrollbar(ScrollbarInfo),
    Metrics(RendererCellMetrics),
}

#[derive(Clone)]
pub struct RendererTextConfig {
    pub font_family: Arc<str>,
    pub font_size: Pixels,
    pub scale_factor: f32,
    /// Bootstrap metrics only. The renderer rebuilds these from DirectWrite and
    /// publishes authoritative values as soon as its thread starts.
    pub cell_width: Pixels,
    pub line_height: Pixels,
    pub baseline: Pixels,
}

pub struct TerminalRenderer {
    host: ExternalSurfaceHost,
    thread: RendererThreadHandle,
    surface_handle: HANDLE,
}

impl Drop for TerminalRenderer {
    fn drop(&mut self) {
        if !self.surface_handle.is_invalid() {
            let _ = unsafe { CloseHandle(self.surface_handle) };
        }
    }
}

impl TerminalRenderer {
    pub fn new(
        window: &Window,
        terminal: Arc<Terminal>,
        text_config: RendererTextConfig,
        ui_tx: Sender<RendererUiUpdate>,
    ) -> Result<Self> {
        let (event_tx, event_rx) = unbounded::<ExternalSurfaceEvent>();
        let host = window
            .create_external_surface_host(event_tx)
            .context("Window::create_external_surface_host() returned None")?;

        let device = create_renderer_device()?;
        let (swap_chain, surface_handle) = create_composition_swap_chain(&device, 1, 1)?;
        host.set_surface_handle(surface_handle)?;

        let thread = spawn(
            device,
            swap_chain,
            terminal,
            text_config,
            ui_tx,
            host.state(),
            event_rx,
        );

        Ok(Self {
            host,
            thread,
            surface_handle,
        })
    }

    pub fn sender(&self) -> crossbeam_channel::Sender<RendererMessage> {
        self.thread.sender()
    }

    pub fn host(&self) -> ExternalSurfaceHost {
        self.host.clone()
    }
}

fn create_renderer_device() -> Result<ID3D11Device> {
    let feature_levels = [
        D3D_FEATURE_LEVEL_12_1,
        D3D_FEATURE_LEVEL_12_0,
        D3D_FEATURE_LEVEL_11_1,
        D3D_FEATURE_LEVEL_11_0,
    ];

    let mut device = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            Default::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&feature_levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
        .context("D3D11CreateDevice failed for renderer")?;
    }

    device.context("D3D11CreateDevice returned null device")
}

fn create_composition_swap_chain(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<(IDXGISwapChain1, HANDLE)> {
    let dxgi_device: IDXGIDevice = device
        .cast()
        .context("Cast ID3D11Device->IDXGIDevice failed")?;
    let adapter: IDXGIAdapter =
        unsafe { dxgi_device.GetAdapter() }.context("IDXGIDevice::GetAdapter failed")?;
    let factory: IDXGIFactory2 =
        unsafe { adapter.GetParent() }.context("IDXGIAdapter::GetParent<IDXGIFactory2> failed")?;
    let factory_media: IDXGIFactoryMedia = factory
        .cast()
        .context("Cast IDXGIFactory2->IDXGIFactoryMedia failed")?;

    // Same access mask WT uses for composition surface handles.
    const COMPOSITIONSURFACE_ALL_ACCESS: u32 = 0x0003;
    let surface_handle =
        unsafe { DCompositionCreateSurfaceHandle(COMPOSITIONSURFACE_ALL_ACCESS, None) }
            .context("DCompositionCreateSurfaceHandle failed")?;

    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: width,
        Height: height,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        Stereo: false.into(),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 3,
        Scaling: DXGI_SCALING_NONE,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
        Flags: DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32,
    };

    let swap_chain = unsafe {
        factory_media.CreateSwapChainForCompositionSurfaceHandle(
            device,
            Some(surface_handle),
            &desc,
            None,
        )
    }
    .context("CreateSwapChainForCompositionSurfaceHandle failed")?;
    Ok((swap_chain, surface_handle))
}
