use anyhow::{Context, Result, anyhow};
use gpui::ExternalSurfaceState;
use std::fmt;
use windows::Win32::Foundation::{HANDLE, HMODULE, RECT, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
    D3D_FEATURE_LEVEL_12_0, D3D_FEATURE_LEVEL_12_1, D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_SHADER_RESOURCE, D3D11_BIND_VERTEX_BUFFER,
    D3D11_BLEND_DESC, D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD,
    D3D11_BUFFER_DESC, D3D11_COLOR_WRITE_ENABLE_ALL, D3D11_COPY_DISCARD, D3D11_CPU_ACCESS_WRITE,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_INPUT_ELEMENT_DESC, D3D11_INPUT_PER_INSTANCE_DATA,
    D3D11_MAP_WRITE_DISCARD, D3D11_RENDER_TARGET_BLEND_DESC, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED,
    D3D11_SDK_VERSION, D3D11_USAGE_DEFAULT, D3D11_USAGE_DYNAMIC, D3D11_VIEWPORT, D3D11CreateDevice,
    ID3D11BlendState, ID3D11Buffer, ID3D11Device5, ID3D11DeviceContext1, ID3D11InputLayout,
    ID3D11PixelShader, ID3D11RenderTargetView, ID3D11ShaderResourceView, ID3D11Texture2D,
    ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R16G16_SINT, DXGI_FORMAT_R16G16_UINT, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_PRESENT, DXGI_PRESENT_PARAMETERS, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIAdapter, IDXGIDevice1, IDXGIFactory2, IDXGISwapChain2,
};
use windows::Win32::System::Threading::WaitForSingleObjectEx;
use windows::core::{Interface as _, s};

use crate::font::atlas::AtlasResources;
use crate::font::types::TextRenderingParams;
use crate::types::{DirtyRect, FrameOutcome, GridSize, QuadInstance};

mod shader {
    include!(concat!(env!("OUT_DIR"), "/renderer_shaders_bytes.rs"));
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderError;

impl std::error::Error for RenderError {}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("D3D11 render failed")
    }
}

#[derive(Clone)]
pub struct GpuContext {
    pub device: ID3D11Device5,
    pub context: ID3D11DeviceContext1,
}

impl GpuContext {
    pub fn new(device: ID3D11Device5) -> Option<Self> {
        let ctx = unsafe { device.GetImmediateContext().ok()? };
        let context: ID3D11DeviceContext1 = ctx.cast().ok()?;

        Some(Self { device, context })
    }
}

pub struct PresentationCtx {
    pub flwo: HANDLE,
    pub swap_chain: IDXGISwapChain2,
}

impl PresentationCtx {
    pub fn new(swap_chain: IDXGISwapChain2) -> Option<Self> {
        unsafe { swap_chain.SetMaximumFrameLatency(1).ok()? };
        let flwo = unsafe { swap_chain.GetFrameLatencyWaitableObject() };
        if flwo.is_invalid() {
            return None;
        }
        Some(Self { swap_chain, flwo })
    }
}

pub struct D3D11 {
    gpu: GpuContext,
    presentation: PresentationCtx,
    resources: Resources,
    pipeline: ShaderPipeline,
    target: Option<RenderTarget>,
    atlas_bind: [Option<ID3D11ShaderResourceView>; 2],
    force_full_presentation: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct BackendOptions {
    pub background_opacity: f32,
}

impl D3D11 {
    pub fn new(options: BackendOptions) -> Result<Self> {
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
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&raw mut device),
                None,
                None,
            )
            .context("D3D11CreateDevice failed for renderer")?;
        };
        let device = device.expect("D3D11CreateDevice returned null device");

        let dxgi_device: IDXGIDevice1 = device.cast()?;
        let adapter: IDXGIAdapter = unsafe { dxgi_device.GetAdapter()? };
        let factory: IDXGIFactory2 = unsafe { adapter.GetParent()? };

        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: 1,
            Height: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 3,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            // Opaque swap chains (DXGI_ALPHA_MODE_IGNORE) can use DWM's independent-flip fast path.
            // Premultiplied alpha forces DWM composition even when every rendered pixel is opaque,
            // substantially increasing DWM GPU work. Windows Terminal documents the same
            // performance and display-latency tradeoff in AtlasEngine.r.cpp::_createSwapChain.
            AlphaMode: match options.background_opacity < 1.0 {
                true => DXGI_ALPHA_MODE_PREMULTIPLIED,
                false => DXGI_ALPHA_MODE_IGNORE,
            },
            Flags: DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32,
        };

        let swap_chain: IDXGISwapChain2 = unsafe {
            let swap_chain_1 =
                factory.CreateSwapChainForComposition(&device, &raw const desc, None)?;
            swap_chain_1.cast()?
        };

        let device: ID3D11Device5 = device.cast()?;
        let pipeline = ShaderPipeline::new(
            &device,
            shader::RENDERER_VERTEX_BYTES,
            shader::RENDERER_BG_FRAGMENT_BYTES,
            shader::RENDERER_FG_FRAGMENT_BYTES,
        )?;
        let resources = Resources::new(&device)?;
        let gpu =
            GpuContext::new(device).ok_or_else(|| anyhow!("Failed to setup GPU resources"))?;
        let presentation = PresentationCtx::new(swap_chain)
            .ok_or_else(|| anyhow!("Failed to setup presentation resources"))?;

        pipeline.bind(&gpu.context);
        resources.bind(&gpu.context);

        Ok(Self {
            target: None,
            gpu,
            presentation,
            pipeline,
            resources,
            atlas_bind: [None, None],
            force_full_presentation: false,
        })
    }

    pub fn swap_chain(&self) -> &IDXGISwapChain2 {
        &self.presentation.swap_chain
    }

    pub fn apply_surface_state(&mut self, state: ExternalSurfaceState) -> Result<bool> {
        let width_px = state.logical_size.width.scale(state.window_scale_factor).0;
        let height_px = state.logical_size.height.scale(state.window_scale_factor).0;
        let (width, height) = (width_px.round() as u32, height_px.round() as u32);

        if width == 0 || height == 0 {
            return Ok(false);
        }

        let needs_resize = self
            .target
            .as_ref()
            .is_none_or(|target| target.width != width || target.height != height);

        if needs_resize {
            self.resize(width, height)?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn prepare(&mut self) {
        let ctx = &self.gpu.context;
        let Some(target) = self.target.as_ref() else {
            std::hint::cold_path();
            return;
        };
        let (width, height) = (target.width as f32, target.height as f32);

        let viewport = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: width,
            Height: height,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        unsafe { ctx.OMSetRenderTargets(Some(&target.rtv_bind), None) };
        unsafe { ctx.RSSetViewports(Some(std::slice::from_ref(&viewport))) };

        self.resources.vs_globals.upload(ctx);
        self.resources.ps_globals.upload(ctx);
        self.resources.cursor_globals.upload(ctx);
    }

    pub fn wait_for_frame(&self) {
        let wait_result = unsafe { WaitForSingleObjectEx(self.presentation.flwo, 100, true) };
        if wait_result == WAIT_OBJECT_0 || wait_result == WAIT_TIMEOUT {
            return;
        }
        // Unexpected wait status
    }

    pub fn sync_instances(&mut self, lists: &[Vec<QuadInstance>], count: u32) -> Result<()> {
        let Some(target) = self.target.as_ref() else {
            std::hint::cold_path();
            return Ok(());
        };

        // +1 for the background instance
        self.resources
            .vertex_buffers
            .reserve(&self.gpu, count + 1)?;
        let instances = &self.resources.vertex_buffers.instances;

        unsafe {
            let mut mapped = std::mem::zeroed();
            self.gpu.context.Map(
                instances,
                0,
                D3D11_MAP_WRITE_DISCARD,
                0,
                Some(&raw mut mapped),
            )?;
            let mut dst: *mut QuadInstance = mapped.pData.cast();

            let bg = QuadInstance::background_rect([target.width as u16, target.height as u16]);
            dst.write(bg);
            dst = dst.add(1);

            for lane in lists {
                let length = lane.len();
                std::ptr::copy_nonoverlapping(lane.as_ptr(), dst, length);
                dst = dst.add(length);
            }

            self.gpu.context.Unmap(instances, 0);
        }
        Ok(())
    }

    pub fn sync_background(&mut self, colors: &[[u8; 4]], generation: u64) -> Result<()> {
        let ctx = &self.gpu;
        self.resources.background.sync(ctx, colors, generation)
    }

    pub fn draw(&mut self, atlases: (&AtlasResources, &AtlasResources), instances: usize) {
        let ctx = &self.gpu.context;

        // Atlases are owned by the rasterizer, so detect replacement here.
        let atlas_slots = [&atlases.0.srv, &atlases.1.srv];
        for (index, (bound, next)) in self.atlas_bind.iter_mut().zip(atlas_slots).enumerate() {
            let bound_ptr = bound.as_ref().map(|srv| srv.as_raw());
            let next_ptr = next.as_ref().map(|srv| srv.as_raw());

            if bound_ptr == next_ptr {
                continue;
            }

            *bound = next.clone();
            let idx = index as u32 + 1;
            unsafe { ctx.PSSetShaderResources(idx, Some(std::slice::from_ref(bound))) };
        }

        unsafe {
            // The background pass initializes every target pixel, including alpha.
            ctx.OMSetBlendState(&self.pipeline.bg_blend_state, None, u32::MAX);
            ctx.PSSetShader(&self.pipeline.bg_pixel_shader, None);
            ctx.DrawInstanced(4, 1, 0, 0);

            // The vertex shader premultiplies foreground colors before blending.
            ctx.OMSetBlendState(&self.pipeline.fg_blend_state, None, u32::MAX);
            ctx.PSSetShader(&self.pipeline.fg_pixel_shader, None);
            ctx.DrawInstanced(4, instances as u32, 0, 1);
        }
    }

    pub fn present(&mut self, damage: Option<DirtyRect>) -> Result<FrameOutcome, RenderError> {
        if damage.is_none() && !self.force_full_presentation {
            return Ok(FrameOutcome::Skipped);
        }

        let Some(target) = self.target.as_ref() else {
            std::hint::cold_path();
            return Ok(FrameOutcome::Skipped);
        };

        let target_width = target.width as i32;
        let target_height = target.height as i32;

        let mut dirty_rect = damage
            .map(|rect| RECT {
                left: 0,
                top: rect.top.clamp(0, target_height),
                right: target_width,
                bottom: rect.bottom.clamp(0, target_height),
            })
            .filter(|rect| rect.top < rect.bottom);

        if dirty_rect.is_none() && !self.force_full_presentation {
            return Ok(FrameOutcome::Skipped);
        }

        let (dirty_rects_count, dirty_rects) = match dirty_rect.as_mut() {
            Some(rect) if !self.force_full_presentation => (1, std::ptr::from_mut(rect)),
            Some(_) | None => (0, std::ptr::null_mut()),
        };
        let params = DXGI_PRESENT_PARAMETERS {
            DirtyRectsCount: dirty_rects_count,
            pDirtyRects: dirty_rects,
            pScrollRect: std::ptr::null_mut(),
            pScrollOffset: std::ptr::null_mut(),
        };

        let swap_chain = &self.presentation.swap_chain;
        if unsafe { swap_chain.Present1(1, DXGI_PRESENT(0), &raw const params) }.is_err() {
            return Err(RenderError);
        }
        self.force_full_presentation = false;
        Ok(FrameOutcome::Presented)
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        unsafe { self.gpu.context.OMSetRenderTargets(None, None) };
        if let Some(rtv) = self.target.as_ref().map(|t| &t.rtv) {
            unsafe { self.gpu.context.DiscardView(rtv) };
        }
        self.target = None;

        unsafe {
            self.presentation.swap_chain.ResizeBuffers(
                0,
                width,
                height,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT,
            )?
        };

        let texture: ID3D11Texture2D = unsafe { self.presentation.swap_chain.GetBuffer(0) }?;
        let mut output = None;
        let device = &self.gpu.device;
        unsafe { device.CreateRenderTargetView(&texture, None, Some(&raw mut output))? };
        let rtv = output.context("Failed to create RTV")?;

        self.target = Some(RenderTarget {
            rtv: rtv.clone(),
            rtv_bind: [Some(rtv)],
            height,
            width,
        });
        self.resources.vs_globals.set(
            |globals| &mut globals.position_scale,
            [2.0 / width as f32, -2.0 / height as f32],
        );
        self.force_full_presentation = true;
        Ok(())
    }

    #[inline]
    pub fn gpu_context(&self) -> &GpuContext {
        &self.gpu
    }

    #[inline]
    pub fn set_background(&mut self, color: [f32; 4]) -> bool {
        self.resources
            .ps_globals
            .set(|globals| &mut globals.background_color, color)
    }

    #[inline]
    pub fn set_cell_size(&mut self, width: f32, height: f32) {
        self.resources.ps_globals.set(
            |globals| &mut globals.cell_size_inv,
            [1.0 / width, 1.0 / height],
        );
    }

    #[inline]
    pub fn set_grid_size(&mut self, size: GridSize) {
        self.resources.ps_globals.set(
            |globals| &mut globals.grid_size,
            [size.columns.into(), size.rows.into()],
        );
    }

    #[inline]
    pub fn set_text_rendering_params(&mut self, params: TextRenderingParams) {
        self.resources
            .vs_globals
            .set(|globals| &mut globals.text_rendering_params, params);
    }

    pub fn set_cursor(&mut self, rect: Option<[f32; 4]>, text_color: [u8; 4]) {
        let Some(rect) = rect else {
            self.resources
                .cursor_globals
                .set(|globals| &mut globals.cursor_rect, [0.0; 4]);
            return;
        };

        let straight = [
            f32::from(text_color[0]) / 255.0,
            f32::from(text_color[1]) / 255.0,
            f32::from(text_color[2]) / 255.0,
        ];
        let alpha = f32::from(text_color[3]) / 255.0;
        let color = [
            straight[0] * alpha,
            straight[1] * alpha,
            straight[2] * alpha,
            alpha,
        ];
        let params = &self.resources.vs_globals.globals.text_rendering_params;
        self.resources.cursor_globals.set(
            |globals| globals,
            CursorGlobals {
                cursor_rect: rect,
                cursor_text_color: color,
                cursor_text_correction: params.correction(straight),
            },
        );
    }
}

struct Resources {
    background: Background,
    // Buffers
    vertex_buffers: VertexBuffers,
    vs_globals: ConstantBuffer<VsGlobals>,
    ps_globals: ConstantBuffer<PsGlobals>,
    cursor_globals: ConstantBuffer<CursorGlobals>,
}

impl Resources {
    const INITIAL_INSTANCE_CAPACITY: u32 = 1024;

    pub fn new(device: &ID3D11Device5) -> Result<Self> {
        Ok(Self {
            background: Background::new(device)?,
            vertex_buffers: VertexBuffers::new(device, Self::INITIAL_INSTANCE_CAPACITY)?,
            vs_globals: ConstantBuffer::new(device, VsGlobals::default())?,
            ps_globals: ConstantBuffer::new(device, PsGlobals::default())?,
            cursor_globals: ConstantBuffer::new(device, CursorGlobals::default())?,
        })
    }

    pub fn bind(&self, ctx: &ID3D11DeviceContext1) {
        self.background.bind(ctx);
        self.vertex_buffers.bind(ctx);
        unsafe {
            ctx.VSSetConstantBuffers(0, Some(&self.vs_globals.bind));
            ctx.VSSetConstantBuffers(1, Some(&self.cursor_globals.bind));
            ctx.PSSetConstantBuffers(0, Some(&self.ps_globals.bind));
            ctx.PSSetConstantBuffers(1, Some(&self.cursor_globals.bind));
        }
    }
}

struct RenderTarget {
    rtv: ID3D11RenderTargetView,
    rtv_bind: [Option<ID3D11RenderTargetView>; 1],
    width: u32,
    height: u32,
}

struct VertexBuffers {
    instances: ID3D11Buffer,
    instance_capacity: u32,
    bind: [Option<ID3D11Buffer>; 1],
}

impl VertexBuffers {
    const INSTANCE_SIZE: u32 = size_of::<QuadInstance>() as u32;

    const VERTEX_DESC: D3D11_BUFFER_DESC = D3D11_BUFFER_DESC {
        ByteWidth: size_of::<QuadInstance>() as u32,
        Usage: D3D11_USAGE_DYNAMIC,
        BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
        MiscFlags: 0,
        StructureByteStride: 0,
    };

    pub fn new(device: &ID3D11Device5, elements: u32) -> Result<Self> {
        let mut desc = Self::VERTEX_DESC;
        desc.ByteWidth *= elements;

        let mut vertex_buf = None;
        unsafe { device.CreateBuffer(&raw const desc, None, Some(&raw mut vertex_buf))? };
        let vertex_buf = vertex_buf.context("CreateBuffer returned null")?;

        Ok(Self {
            instances: vertex_buf.clone(),
            instance_capacity: elements,
            bind: [Some(vertex_buf)],
        })
    }

    fn bind(&self, ctx: &ID3D11DeviceContext1) {
        let strides = [Self::INSTANCE_SIZE];
        let offsets = [0];
        unsafe {
            ctx.IASetVertexBuffers(
                0,
                1,
                Some(self.bind.as_ptr()),
                Some(strides.as_ptr()),
                Some(offsets.as_ptr()),
            );
        }
    }

    pub fn reserve(&mut self, gpu: &GpuContext, capacity: u32) -> Result<()> {
        if capacity <= self.instance_capacity {
            return Ok(());
        }

        let needed = capacity.saturating_mul(Self::INSTANCE_SIZE).max(64 * 1024);
        // Round up to the next 64KB boundary because D3D11 allocates 64KB pages
        let aligned_size = (needed + (64 * 1024 - 1)) & !(64 * 1024 - 1);

        let mut desc = Self::VERTEX_DESC;
        desc.ByteWidth = aligned_size;

        let mut vertex_buf = None;
        unsafe {
            gpu.device
                .CreateBuffer(&raw const desc, None, Some(&raw mut vertex_buf))?
        };
        let vertex_buf = vertex_buf.context("CreateBuffer returned null")?;

        self.instances = vertex_buf.clone();
        self.bind[0] = Some(vertex_buf);
        self.instance_capacity = aligned_size / Self::INSTANCE_SIZE;
        self.bind(&gpu.context);
        Ok(())
    }
}

#[repr(C)]
#[derive(Default, PartialEq)]
struct VsGlobals {
    position_scale: [f32; 2],
    _pad: [f32; 2],
    text_rendering_params: TextRenderingParams,
}

struct ConstantBuffer<T> {
    buffer: ID3D11Buffer,
    bind: [Option<ID3D11Buffer>; 1],
    globals: T,
    dirty: bool,
}

impl<T> ConstantBuffer<T> {
    fn new(device: &ID3D11Device5, globals: T) -> Result<Self> {
        const { assert!(size_of::<T>() % 16 == 0) };

        let desc = D3D11_BUFFER_DESC {
            ByteWidth: size_of::<T>() as u32,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
            StructureByteStride: 0,
        };

        let mut buffer = None;
        unsafe { device.CreateBuffer(&raw const desc, None, Some(&raw mut buffer))? };
        let buffer = buffer.context("CreateBuffer returned null")?;

        Ok(Self {
            buffer: buffer.clone(),
            bind: [Some(buffer)],
            globals,
            dirty: true,
        })
    }

    #[inline]
    fn set<U: PartialEq>(
        &mut self,
        field: impl for<'a> FnOnce(&'a mut T) -> &'a mut U,
        value: U,
    ) -> bool {
        let field = field(&mut self.globals);
        if *field == value {
            return false;
        }

        *field = value;
        self.dirty = true;
        true
    }

    fn upload(&mut self, ctx: &ID3D11DeviceContext1) {
        if !self.dirty {
            return;
        }

        unsafe {
            ctx.UpdateSubresource1(
                &self.buffer,
                0,
                None,
                std::ptr::from_ref(&self.globals).cast(),
                size_of::<T>() as u32,
                0,
                D3D11_COPY_DISCARD.0 as u32,
            );
        }
        self.dirty = false;
    }
}

#[repr(C)]
#[derive(Default, PartialEq)]
struct PsGlobals {
    background_color: [f32; 4],
    cell_size_inv: [f32; 2],
    grid_size: [u32; 2],
}

#[repr(C)]
#[derive(Default, PartialEq)]
struct CursorGlobals {
    cursor_rect: [f32; 4],
    cursor_text_color: [f32; 4],
    cursor_text_correction: [f32; 4],
}

const _: () = assert!(size_of::<CursorGlobals>() == 48);

struct ShaderPipeline {
    vertex_shader: ID3D11VertexShader,
    bg_pixel_shader: ID3D11PixelShader,
    fg_pixel_shader: ID3D11PixelShader,
    input_layout: ID3D11InputLayout,
    bg_blend_state: ID3D11BlendState,
    fg_blend_state: ID3D11BlendState,
}

impl ShaderPipeline {
    pub fn new(
        device: &ID3D11Device5,
        vertex_bytes: &[u8],
        bg_fragment_bytes: &[u8],
        fg_fragment_bytes: &[u8],
    ) -> Result<Self> {
        let mut shader = None;
        unsafe { device.CreateVertexShader(vertex_bytes, None, Some(&raw mut shader))? };
        let vertex_shader = shader.context("CreateVertexShader returned null")?;

        let mut shader = None;
        unsafe { device.CreatePixelShader(bg_fragment_bytes, None, Some(&raw mut shader))? };
        let bg_pixel_shader = shader.context("CreatePixelShader returned null")?;

        let mut shader = None;
        unsafe { device.CreatePixelShader(fg_fragment_bytes, None, Some(&raw mut shader))? };
        let fg_pixel_shader = shader.context("CreatePixelShader returned null")?;

        // Create the input layout
        let elements = [
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: s!("position"),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R16G16_SINT,
                InputSlot: 0,
                AlignedByteOffset: 0,
                InputSlotClass: D3D11_INPUT_PER_INSTANCE_DATA,
                InstanceDataStepRate: 1,
            },
            D3D11_INPUT_ELEMENT_DESC {
                // Packed `Data` containing size and shading type.
                SemanticName: s!("packed_data"),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R16G16_UINT,
                InputSlot: 0,
                AlignedByteOffset: 4,
                InputSlotClass: D3D11_INPUT_PER_INSTANCE_DATA,
                InstanceDataStepRate: 1,
            },
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: s!("texcoord"),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R16G16_UINT,
                InputSlot: 0,
                AlignedByteOffset: 8,
                InputSlotClass: D3D11_INPUT_PER_INSTANCE_DATA,
                InstanceDataStepRate: 1,
            },
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: s!("color"),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                InputSlot: 0,
                AlignedByteOffset: 12,
                InputSlotClass: D3D11_INPUT_PER_INSTANCE_DATA,
                InstanceDataStepRate: 1,
            },
        ];

        let mut layout = None;
        unsafe { device.CreateInputLayout(&elements, vertex_bytes, Some(&raw mut layout))? };
        let input_layout = layout.context("CreateInputLayout returned null")?;

        let mut bg_desc = D3D11_BLEND_DESC::default();
        bg_desc.RenderTarget[0].RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8;
        let mut bg_blend_state = None;
        unsafe { device.CreateBlendState(&raw const bg_desc, Some(&raw mut bg_blend_state))? };
        let bg_blend_state = bg_blend_state.context("CreateBlendState returned null")?;

        let mut fg_desc = D3D11_BLEND_DESC::default();
        fg_desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
            BlendEnable: true.into(),
            BlendOp: D3D11_BLEND_OP_ADD,
            BlendOpAlpha: D3D11_BLEND_OP_ADD,
            SrcBlend: D3D11_BLEND_ONE,
            SrcBlendAlpha: D3D11_BLEND_ONE,
            DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
            DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
            RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
        };
        let mut fg_blend_state = None;
        unsafe { device.CreateBlendState(&raw const fg_desc, Some(&raw mut fg_blend_state))? };
        let fg_blend_state = fg_blend_state.context("CreateBlendState returned null")?;

        Ok(Self {
            vertex_shader,
            bg_pixel_shader,
            fg_pixel_shader,
            input_layout,
            bg_blend_state,
            fg_blend_state,
        })
    }

    pub fn bind(&self, ctx: &ID3D11DeviceContext1) {
        unsafe {
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            ctx.IASetInputLayout(&self.input_layout);
            ctx.VSSetShader(&self.vertex_shader, None);
        }
    }
}

struct Background {
    buffer: ID3D11Buffer,
    srv: Option<ID3D11ShaderResourceView>,
    capacity: usize,
    generation: u64,
}

impl Background {
    const BUFFER_DESC: D3D11_BUFFER_DESC = D3D11_BUFFER_DESC {
        ByteWidth: 4,
        Usage: D3D11_USAGE_DYNAMIC,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
        MiscFlags: D3D11_RESOURCE_MISC_BUFFER_STRUCTURED.0 as u32,
        StructureByteStride: size_of::<[u8; 4]>() as u32,
    };

    pub fn new(device: &ID3D11Device5) -> Result<Self> {
        let buffer = Self::create_buffer(device, 1)?;
        let mut srv = None;
        unsafe { device.CreateShaderResourceView(&buffer, None, Some(&raw mut srv))? };

        Ok(Self {
            buffer,
            capacity: 1,
            srv,
            generation: 0,
        })
    }

    fn create_buffer(device: &ID3D11Device5, capacity: usize) -> Result<ID3D11Buffer> {
        let byte_width = capacity
            .checked_mul(size_of::<[u8; 4]>())
            .context("background buffer size overflow")? as u32;

        let mut desc = Self::BUFFER_DESC;
        desc.ByteWidth = byte_width.max(4);

        let mut buffer = None;
        unsafe { device.CreateBuffer(&desc, None, Some(&mut buffer))? };
        buffer.context("CreateBuffer background cells returned null")
    }

    fn bind(&self, ctx: &ID3D11DeviceContext1) {
        unsafe { ctx.PSSetShaderResources(0, Some(std::slice::from_ref(&self.srv))) };
    }

    fn reserve(&mut self, gpu: &GpuContext, capacity: usize) -> Result<()> {
        if capacity <= self.capacity {
            return Ok(());
        }

        let next = capacity.next_power_of_two();
        let buffer = Self::create_buffer(&gpu.device, next)?;
        let mut srv = None;
        unsafe {
            gpu.device
                .CreateShaderResourceView(&buffer, None, Some(&raw mut srv))?
        };

        self.buffer = buffer;
        self.srv = srv;
        self.capacity = next;
        self.bind(&gpu.context);
        Ok(())
    }

    #[inline]
    pub fn sync(&mut self, gpu: &GpuContext, bg: &[[u8; 4]], bg_gen: u64) -> Result<()> {
        if self.generation == bg_gen {
            return Ok(());
        }

        self.reserve(gpu, bg.len())?;
        unsafe {
            let mut mapped = std::mem::zeroed();
            gpu.context.Map(
                &self.buffer,
                0,
                D3D11_MAP_WRITE_DISCARD,
                0,
                Some(&raw mut mapped),
            )?;
            std::ptr::copy_nonoverlapping(bg.as_ptr(), mapped.pData.cast::<[u8; 4]>(), bg.len());
            gpu.context.Unmap(&self.buffer, 0);
        }

        self.generation = bg_gen;
        Ok(())
    }
}
