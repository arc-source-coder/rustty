use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable, cast_slice};
use font::cache::glyph_cache::GlyphAtlasKind;
use font::shared_grid::SharedGrid;
use font::shared_grid_set::SharedGridPtr;
use gpui::ExternalSurfaceState;
use std::mem::{size_of, size_of_val};
use std::slice;
use windows::Win32::Foundation::{HANDLE, RECT, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_INDEX_BUFFER, D3D11_BIND_SHADER_RESOURCE,
    D3D11_BIND_VERTEX_BUFFER, D3D11_BLEND_DESC, D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_ONE,
    D3D11_BLEND_OP_ADD, D3D11_BUFFER_DESC, D3D11_COLOR_WRITE_ENABLE_ALL, D3D11_COMPARISON_ALWAYS,
    D3D11_CPU_ACCESS_WRITE, D3D11_FILTER_MIN_MAG_MIP_POINT, D3D11_FLOAT32_MAX,
    D3D11_INPUT_ELEMENT_DESC, D3D11_INPUT_PER_INSTANCE_DATA, D3D11_INPUT_PER_VERTEX_DATA,
    D3D11_MAP_WRITE_DISCARD, D3D11_SAMPLER_DESC, D3D11_SUBRESOURCE_DATA,
    D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_DYNAMIC,
    D3D11_VIEWPORT, ID3D11BlendState, ID3D11Buffer, ID3D11Device, ID3D11DeviceContext,
    ID3D11InputLayout, ID3D11PixelShader, ID3D11RenderTargetView, ID3D11SamplerState,
    ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UINT,
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R16_UINT, DXGI_FORMAT_R16G16_SINT,
    DXGI_FORMAT_R16G16_UINT, DXGI_FORMAT_R32G32_FLOAT, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_PRESENT, DXGI_PRESENT_PARAMETERS, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT, IDXGISwapChain1, IDXGISwapChain2,
};
use windows::Win32::System::Threading::WaitForSingleObjectEx;
use windows::core::{Interface, s};

use super::shared_grid_ptr::shared_grid_ref;
use super::types::{QuadInstance, RenderBatch};

mod shader_bytes {
    include!(concat!(env!("OUT_DIR"), "/renderer_shaders_bytes.rs"));
}

const RENDERER_SWAP_CHAIN_FLAGS: i32 = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0;
const INITIAL_INSTANCE_CAPACITY: usize = 1024;

pub(crate) struct D3D11Backend {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    swap_chain: IDXGISwapChain1,
    atlas_grayscale: AtlasTexture,
    atlas_color: AtlasTexture,
    pipeline: GlyphPipeline,
    background_cells: BackgroundCells,
    target: Option<RenderTarget>,
    surface_state: ExternalSurfaceState,
    frame_latency_waitable_object: Option<HANDLE>,
    present_rect_scratch: Vec<RECT>,
}

#[derive(Clone)]
struct RenderTarget {
    _texture: ID3D11Texture2D,
    rtv: ID3D11RenderTargetView,
    rtv_bind: [Option<ID3D11RenderTargetView>; 1],
    width: u32,
    height: u32,
}

struct AtlasTexture {
    texture: ID3D11Texture2D,
    srv: Option<ID3D11ShaderResourceView>,
    side: u32,
    modified_seen: u64,
    resized_seen: u64,
    format: DXGI_FORMAT,
    bytes_per_pixel: u32,
}

struct GlyphPipeline {
    vertex_shader: ID3D11VertexShader,
    pixel_shader: ID3D11PixelShader,
    input_layout: ID3D11InputLayout,
    index_buffer: ID3D11Buffer,
    blend_state: ID3D11BlendState,
    globals_buffer: ID3D11Buffer,
    instance_buffer: ID3D11Buffer,
    instance_capacity: usize,
    vertex_buffers_bind: [Option<ID3D11Buffer>; 2],
    globals_bind: [Option<ID3D11Buffer>; 1],
    sampler_bind: [Option<ID3D11SamplerState>; 1],
    shader_resources_bind: [Option<ID3D11ShaderResourceView>; 3],
    shader_resources_dirty: bool,
    last_globals: Option<GlyphGlobals>,
    static_state_dirty: bool,
}

struct BackgroundCells {
    texture: ID3D11Texture2D,
    srv: Option<ID3D11ShaderResourceView>,
    cols: u16,
    rows: u16,
    generation: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, PartialEq)]
struct GlyphGlobals {
    position_scale: [f32; 2],
    _pad0: [f32; 2],
    background_color: [f32; 4],
    background_cell_size: [f32; 2],
    background_cell_count: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GlyphVertex {
    unit: [f32; 2],
}

impl D3D11Backend {
    pub(crate) fn new(
        device: ID3D11Device,
        swap_chain: IDXGISwapChain1,
        shared_grid: SharedGridPtr,
    ) -> Result<Self> {
        let context =
            unsafe { device.GetImmediateContext() }.context("GetImmediateContext failed")?;
        let atlas_grayscale = AtlasTexture::new(
            &device,
            shared_grid_ref(shared_grid),
            GlyphAtlasKind::Grayscale,
        )?;
        let atlas_color =
            AtlasTexture::new(&device, shared_grid_ref(shared_grid), GlyphAtlasKind::Color)?;
        let pipeline = GlyphPipeline::new(&device)?;
        let background_cells = BackgroundCells::new(&device)?;
        let frame_latency_waitable_object = configure_frame_latency_waitable_object(&swap_chain);
        Ok(Self {
            device,
            context,
            swap_chain,
            atlas_grayscale,
            atlas_color,
            pipeline,
            background_cells,
            target: None,
            surface_state: ExternalSurfaceState::default(),
            frame_latency_waitable_object,
            present_rect_scratch: Vec::new(),
        })
    }

    pub(crate) fn has_target(&self) -> bool {
        self.target.is_some() && self.surface_state.visible
    }

    pub(crate) fn draw_and_present(
        &mut self,
        shared_grid: SharedGridPtr,
        batch: &RenderBatch,
        fg_lists: &[Vec<QuadInstance>],
        bg_cells: &[u32],
    ) -> Result<()> {
        let Some(target) = self.target.as_ref() else {
            return Ok(());
        };
        self.begin_target_frame(target, batch.clear_color);
        let grayscale_resized = self.atlas_grayscale.sync(
            &self.device,
            &self.context,
            shared_grid_ref(shared_grid),
            GlyphAtlasKind::Grayscale,
        )?;
        let color_resized = self.atlas_color.sync(
            &self.device,
            &self.context,
            shared_grid_ref(shared_grid),
            GlyphAtlasKind::Color,
        )?;
        let bg_resized = self.background_cells.update_from_batch(
            &self.device,
            &self.context,
            batch,
            bg_cells,
        )?;
        if grayscale_resized || color_resized || bg_resized {
            self.pipeline.mark_shader_resources_dirty();
        }
        self.pipeline.draw(
            &self.device,
            &self.context,
            target,
            &self.background_cells,
            batch,
            &self.atlas_grayscale,
            &self.atlas_color,
            fg_lists,
            batch.instance_count,
        )?;
        self.present(batch)?;
        Ok(())
    }

    pub(crate) fn apply_surface_state(&mut self, next_state: ExternalSurfaceState) -> Result<bool> {
        let previous_state = self.surface_state;
        self.surface_state = next_state;

        if !next_state.visible {
            return Ok(false);
        }

        let width = next_state.device_size.width.0.max(1) as u32;
        let height = next_state.device_size.height.0.max(1) as u32;
        let needs_resize = self
            .target
            .as_ref()
            .is_none_or(|target| target.width != width || target.height != height);
        if needs_resize {
            self.resize(width, height)?;
            return Ok(true);
        }
        Ok(!previous_state.visible)
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        unsafe {
            self.context.OMSetRenderTargets(None, None);
            self.context.ClearState();
        }
        self.pipeline.mark_state_dirty();
        self.pipeline.mark_shader_resources_dirty();
        self.target = None;

        unsafe {
            self.swap_chain.ResizeBuffers(
                0,
                width,
                height,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_SWAP_CHAIN_FLAG(RENDERER_SWAP_CHAIN_FLAGS),
            )
        }
        .context("ResizeBuffers failed")?;

        let (texture, rtv) = create_render_target_view(&self.swap_chain, &self.device)?;
        let rtv_bind = [Some(rtv.clone())];
        self.target = Some(RenderTarget {
            _texture: texture,
            rtv,
            rtv_bind,
            width,
            height,
        });
        Ok(())
    }

    pub(crate) fn wait_for_frame_latency(&self) {
        let Some(waitable_object) = self.frame_latency_waitable_object else {
            return;
        };
        let wait_result = unsafe { WaitForSingleObjectEx(waitable_object, 100, true) };
        if wait_result == WAIT_OBJECT_0 || wait_result == WAIT_TIMEOUT {
            return;
        }
        log::warn!("frame latency wait returned unexpected status: {wait_result:?}");
    }

    fn begin_target_frame(&self, target: &RenderTarget, clear_color: [f32; 4]) {
        unsafe {
            let mut clear = clear_color;
            clear[3] = 1.0;
            let viewport = D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: target.width as f32,
                Height: target.height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            self.context
                .OMSetRenderTargets(Some(&target.rtv_bind), None);
            self.context.RSSetViewports(Some(&[viewport]));
            self.context.ClearRenderTargetView(&target.rtv, &clear);
        }
    }

    fn present(&mut self, batch: &RenderBatch) -> Result<()> {
        self.present_rect_scratch.clear();
        self.present_rect_scratch.reserve(batch.dirty_rects.len());
        for rect in &batch.dirty_rects {
            if rect.right <= rect.left || rect.bottom <= rect.top {
                continue;
            }
            self.present_rect_scratch.push(RECT {
                left: rect.left,
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
            });
        }

        let params = DXGI_PRESENT_PARAMETERS {
            DirtyRectsCount: self.present_rect_scratch.len() as u32,
            pDirtyRects: if self.present_rect_scratch.is_empty() {
                std::ptr::null_mut()
            } else {
                self.present_rect_scratch.as_mut_ptr()
            },
            pScrollRect: std::ptr::null_mut(),
            pScrollOffset: std::ptr::null_mut(),
        };

        unsafe {
            let hr = self.swap_chain.Present1(1, DXGI_PRESENT(0), &params);
            if hr.is_ok() {
                return Ok(());
            }
            self.swap_chain
                .Present(1, DXGI_PRESENT(0))
                .ok()
                .context("Present failed")
        }
    }
}

impl AtlasTexture {
    fn new(device: &ID3D11Device, shared_grid: &SharedGrid, kind: GlyphAtlasKind) -> Result<Self> {
        let side = shared_grid.with_atlas_snapshot(kind, |atlas| atlas.size);
        let (format, bytes_per_pixel) = match kind {
            GlyphAtlasKind::Grayscale => (DXGI_FORMAT_R8_UNORM, 1),
            GlyphAtlasKind::Color => (DXGI_FORMAT_B8G8R8A8_UNORM, 4),
        };
        let texture = create_texture(
            device,
            side,
            side,
            format,
            D3D11_BIND_SHADER_RESOURCE.0 as u32,
        )?;
        let srv = create_shader_resource_view(device, &texture)?;
        Ok(Self {
            texture,
            srv,
            side,
            modified_seen: 0,
            resized_seen: 0,
            format,
            bytes_per_pixel,
        })
    }

    fn sync(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        shared_grid: &SharedGrid,
        kind: GlyphAtlasKind,
    ) -> Result<bool> {
        let mut srv_resized = false;

        let modified = shared_grid.with_atlas_snapshot(kind, |atlas| atlas.modified);
        if modified == self.modified_seen {
            return Ok(false);
        }

        shared_grid.with_atlas_snapshot(kind, |atlas| -> Result<()> {
            let resized = atlas.resized;
            let size = atlas.size;
            if resized != self.resized_seen || size != self.side {
                self.side = size;
                self.texture = create_texture(
                    device,
                    self.side,
                    self.side,
                    self.format,
                    D3D11_BIND_SHADER_RESOURCE.0 as u32,
                )?;
                self.srv = create_shader_resource_view(device, &self.texture)?;
                self.resized_seen = resized;
                self.modified_seen = 0;
                srv_resized = true;
            }

            let modified_after = atlas.modified;
            if modified_after == self.modified_seen {
                return Ok(());
            }

            unsafe {
                context.UpdateSubresource(
                    &self.texture,
                    0,
                    None,
                    atlas.data.as_ptr() as _,
                    self.side * self.bytes_per_pixel,
                    0,
                );
            }
            self.modified_seen = modified_after;
            Ok(())
        })?;
        Ok(srv_resized)
    }
}

impl BackgroundCells {
    fn new(device: &ID3D11Device) -> Result<Self> {
        let texture = create_dynamic_texture(device, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM)?;
        let srv = create_shader_resource_view(device, &texture)?;
        Ok(Self {
            texture,
            srv,
            cols: 1,
            rows: 1,
            generation: 0,
        })
    }

    fn update_from_batch(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        batch: &RenderBatch,
        bg_cells: &[u32],
    ) -> Result<bool> {
        let cols = batch.grid_cols.max(1);
        let rows = batch.grid_rows.max(1);
        let mut srv_resized = false;
        if self.cols != cols || self.rows != rows {
            self.texture = create_dynamic_texture(
                device,
                cols as u32,
                rows as u32,
                DXGI_FORMAT_R8G8B8A8_UNORM,
            )?;
            self.srv = create_shader_resource_view(device, &self.texture)?;
            self.cols = cols;
            self.rows = rows;
            self.generation = 0;
            srv_resized = true;
        }

        if batch.bg_generation == self.generation {
            return Ok(srv_resized);
        }

        let row_bytes = cols as usize * size_of::<u32>();
        let src = cast_slice(bg_cells);
        let needed_bytes = row_bytes.saturating_mul(rows as usize);
        if src.len() < needed_bytes {
            return Ok(srv_resized);
        }
        unsafe {
            let mut mapped = std::mem::zeroed();
            context.Map(
                &self.texture,
                0,
                D3D11_MAP_WRITE_DISCARD,
                0,
                Some(&mut mapped),
            )?;
            let mut dst = mapped.pData as *mut u8;
            let dst_pitch = mapped.RowPitch as usize;
            for row in 0..rows as usize {
                let src_start = row * row_bytes;
                let src_end = src_start + row_bytes;
                std::ptr::copy_nonoverlapping(src[src_start..src_end].as_ptr(), dst, row_bytes);
                dst = dst.add(dst_pitch);
            }
            context.Unmap(&self.texture, 0);
        }

        self.generation = batch.bg_generation;
        Ok(srv_resized)
    }
}

impl GlyphPipeline {
    fn new(device: &ID3D11Device) -> Result<Self> {
        let vertex_shader = create_vertex_shader(device, shader_bytes::RENDERER_VERTEX_BYTES)?;
        let pixel_shader = create_pixel_shader(device, shader_bytes::RENDERER_FRAGMENT_BYTES)?;
        let input_layout = create_glyph_input_layout(device, shader_bytes::RENDERER_VERTEX_BYTES)?;
        let vertex_buffer = create_static_vertex_buffer(
            device,
            &[
                GlyphVertex { unit: [0.0, 0.0] },
                GlyphVertex { unit: [1.0, 0.0] },
                GlyphVertex { unit: [1.0, 1.0] },
                GlyphVertex { unit: [0.0, 1.0] },
            ],
        )?;
        let index_buffer = create_static_index_buffer(device, &[0_u16, 1, 2, 2, 3, 0])?;
        let blend_state = create_blend_state(device)?;
        let sampler = create_sampler(device)?;
        let globals_buffer = create_dynamic_constant_buffer::<GlyphGlobals>(device)?;
        let instance_buffer =
            create_dynamic_vertex_buffer::<QuadInstance>(device, INITIAL_INSTANCE_CAPACITY)?;
        let vertex_buffers_bind = [Some(vertex_buffer.clone()), Some(instance_buffer.clone())];
        let globals_bind = [Some(globals_buffer.clone())];
        let sampler_bind = [sampler.clone()];

        Ok(Self {
            vertex_shader,
            pixel_shader,
            input_layout,
            index_buffer,
            blend_state,
            globals_buffer,
            instance_buffer,
            instance_capacity: INITIAL_INSTANCE_CAPACITY,
            vertex_buffers_bind,
            globals_bind,
            sampler_bind,
            shader_resources_bind: [None, None, None],
            shader_resources_dirty: true,
            last_globals: None,
            static_state_dirty: true,
        })
    }

    fn mark_state_dirty(&mut self) {
        self.static_state_dirty = true;
    }

    fn mark_shader_resources_dirty(&mut self) {
        self.shader_resources_dirty = true;
    }

    #[allow(clippy::too_many_arguments)]
    fn draw(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        target: &RenderTarget,
        background_cells: &BackgroundCells,
        batch: &RenderBatch,
        atlas_grayscale: &AtlasTexture,
        atlas_color: &AtlasTexture,
        fg_lists: &[Vec<QuadInstance>],
        instance_count: usize,
    ) -> Result<()> {
        let total_instances = instance_count + 1;
        self.ensure_instance_capacity(device, total_instances)?;
        let globals = GlyphGlobals {
            position_scale: [2.0 / target.width as f32, -2.0 / target.height as f32],
            _pad0: [0.0, 0.0],
            background_color: batch.clear_color,
            background_cell_size: batch.cell_size,
            background_cell_count: [background_cells.cols as f32, background_cells.rows as f32],
        };
        if self.last_globals != Some(globals) {
            write_buffer(context, &self.globals_buffer, cast_slice(&[globals]))?;
            self.last_globals = Some(globals);
        }
        self.sync_from_array_lists(context, target, fg_lists)?;

        unsafe {
            if self.static_state_dirty {
                let strides = [size_of::<GlyphVertex>() as u32, size_of::<QuadInstance>() as u32];
                let offsets = [0_u32, 0_u32];
                context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                context.IASetInputLayout(Some(&self.input_layout));
                context.IASetVertexBuffers(
                    0,
                    self.vertex_buffers_bind.len() as u32,
                    Some(self.vertex_buffers_bind.as_ptr()),
                    Some(strides.as_ptr()),
                    Some(offsets.as_ptr()),
                );
                context.IASetIndexBuffer(&self.index_buffer, DXGI_FORMAT_R16_UINT, 0);
                context.VSSetShader(&self.vertex_shader, None);
                context.PSSetShader(&self.pixel_shader, None);
                context.VSSetConstantBuffers(0, Some(&self.globals_bind));
                context.PSSetConstantBuffers(0, Some(&self.globals_bind));
                context.OMSetBlendState(&self.blend_state, None, 0xFFFF_FFFF);
                context.PSSetSamplers(0, Some(&self.sampler_bind));
            }

            self.sync_shader_resource_bindings(
                context,
                background_cells,
                atlas_grayscale,
                atlas_color,
            );
            context.DrawIndexedInstanced(6, total_instances as u32, 0, 0, 0);
        }
        self.static_state_dirty = false;

        Ok(())
    }

    fn sync_shader_resource_bindings(
        &mut self,
        context: &ID3D11DeviceContext,
        background_cells: &BackgroundCells,
        atlas_grayscale: &AtlasTexture,
        atlas_color: &AtlasTexture,
    ) {
        if !self.static_state_dirty && !self.shader_resources_dirty {
            return;
        }
        self.shader_resources_bind = [
            background_cells.srv.clone(),
            atlas_grayscale.srv.clone(),
            atlas_color.srv.clone(),
        ];
        unsafe {
            context.PSSetShaderResources(0, Some(&self.shader_resources_bind));
        }
        self.shader_resources_dirty = false;
    }

    fn sync_from_array_lists(
        &self,
        context: &ID3D11DeviceContext,
        target: &RenderTarget,
        fg_lists: &[Vec<QuadInstance>],
    ) -> Result<()> {
        unsafe {
            let mut mapped = std::mem::zeroed();
            context.Map(
                &self.instance_buffer,
                0,
                D3D11_MAP_WRITE_DISCARD,
                0,
                Some(&mut mapped),
            )?;
            let mut dst = mapped.pData as *mut u8;
            let background =
                QuadInstance::background_rect([target.width as f32, target.height as f32]);
            let background_bytes = cast_slice(slice::from_ref(&background));
            std::ptr::copy_nonoverlapping(background_bytes.as_ptr(), dst, background_bytes.len());
            dst = dst.add(background_bytes.len());
            for lane in fg_lists {
                let lane_slice = lane.as_slice();
                if lane_slice.is_empty() {
                    continue;
                }
                let lane_bytes = cast_slice(lane_slice);
                std::ptr::copy_nonoverlapping(lane_bytes.as_ptr(), dst, lane_bytes.len());
                dst = dst.add(lane_bytes.len());
            }
            context.Unmap(&self.instance_buffer, 0);
        }

        Ok(())
    }

    fn ensure_instance_capacity(&mut self, device: &ID3D11Device, needed: usize) -> Result<()> {
        if needed <= self.instance_capacity {
            return Ok(());
        }

        let instance_size = size_of::<QuadInstance>();
        let needed_bytes = needed.saturating_mul(instance_size).max(64 * 1024);
        let aligned_bytes = (needed_bytes + (64 * 1024 - 1)) & !(64 * 1024 - 1);
        let next = (aligned_bytes / instance_size).max(needed);

        let buffer = create_dynamic_vertex_buffer::<QuadInstance>(device, next)?;
        self.instance_buffer = buffer;
        self.vertex_buffers_bind[1] = Some(self.instance_buffer.clone());
        self.instance_capacity = next;
        self.mark_state_dirty();
        Ok(())
    }
}

fn create_render_target_view(
    swap_chain: &IDXGISwapChain1,
    device: &ID3D11Device,
) -> Result<(ID3D11Texture2D, ID3D11RenderTargetView)> {
    let texture: ID3D11Texture2D =
        unsafe { swap_chain.GetBuffer(0) }.context("GetBuffer(0) failed")?;
    let rtv = unsafe {
        let mut output = None;
        device
            .CreateRenderTargetView(&texture, None, Some(&mut output))
            .context("CreateRenderTargetView failed")?;
        output.context("CreateRenderTargetView returned null")?
    };
    Ok((texture, rtv))
}

fn configure_frame_latency_waitable_object(swap_chain: &IDXGISwapChain1) -> Option<HANDLE> {
    let Ok(swap_chain2): Result<IDXGISwapChain2, _> = swap_chain.cast() else {
        return None;
    };

    if let Err(err) = unsafe { swap_chain2.SetMaximumFrameLatency(1) } {
        log::warn!("SetMaximumFrameLatency(1) failed: {err:#}");
        return None;
    }

    let waitable_object = unsafe { swap_chain2.GetFrameLatencyWaitableObject() };
    if waitable_object.is_invalid() {
        return None;
    }
    Some(waitable_object)
}

fn create_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    bind_flags: u32,
) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind_flags,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    unsafe {
        let mut texture = None;
        device.CreateTexture2D(&desc, None, Some(&mut texture))?;
        texture.context("CreateTexture2D returned null")
    }
}

fn create_dynamic_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DYNAMIC,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
        MiscFlags: 0,
    };
    unsafe {
        let mut texture = None;
        device.CreateTexture2D(&desc, None, Some(&mut texture))?;
        texture.context("CreateTexture2D returned null")
    }
}

fn create_shader_resource_view(
    device: &ID3D11Device,
    resource: &ID3D11Texture2D,
) -> Result<Option<ID3D11ShaderResourceView>> {
    unsafe {
        let mut view = None;
        device.CreateShaderResourceView(resource, None, Some(&mut view))?;
        Ok(view)
    }
}

fn create_vertex_shader(device: &ID3D11Device, bytes: &[u8]) -> Result<ID3D11VertexShader> {
    unsafe {
        let mut shader = None;
        device.CreateVertexShader(bytes, None, Some(&mut shader))?;
        shader.context("CreateVertexShader returned null")
    }
}

fn create_pixel_shader(device: &ID3D11Device, bytes: &[u8]) -> Result<ID3D11PixelShader> {
    unsafe {
        let mut shader = None;
        device.CreatePixelShader(bytes, None, Some(&mut shader))?;
        shader.context("CreatePixelShader returned null")
    }
}

fn create_blend_state(device: &ID3D11Device) -> Result<ID3D11BlendState> {
    let mut desc = D3D11_BLEND_DESC::default();
    desc.RenderTarget[0].BlendEnable = true.into();
    desc.RenderTarget[0].BlendOp = D3D11_BLEND_OP_ADD;
    desc.RenderTarget[0].BlendOpAlpha = D3D11_BLEND_OP_ADD;
    desc.RenderTarget[0].SrcBlend = D3D11_BLEND_ONE;
    desc.RenderTarget[0].SrcBlendAlpha = D3D11_BLEND_ONE;
    desc.RenderTarget[0].DestBlend = D3D11_BLEND_INV_SRC_ALPHA;
    desc.RenderTarget[0].DestBlendAlpha = D3D11_BLEND_INV_SRC_ALPHA;
    desc.RenderTarget[0].RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8;

    unsafe {
        let mut state = None;
        device.CreateBlendState(&desc, Some(&mut state))?;
        state.context("CreateBlendState returned null")
    }
}

fn create_sampler(device: &ID3D11Device) -> Result<Option<ID3D11SamplerState>> {
    let desc = D3D11_SAMPLER_DESC {
        Filter: D3D11_FILTER_MIN_MAG_MIP_POINT,
        AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
        MipLODBias: 0.0,
        MaxAnisotropy: 1,
        ComparisonFunc: D3D11_COMPARISON_ALWAYS,
        BorderColor: [0.0; 4],
        MinLOD: 0.0,
        MaxLOD: D3D11_FLOAT32_MAX,
    };
    unsafe {
        let mut output = None;
        device.CreateSamplerState(&desc, Some(&mut output))?;
        Ok(output)
    }
}

fn create_dynamic_constant_buffer<T>(device: &ID3D11Device) -> Result<ID3D11Buffer> {
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: size_of::<T>() as u32,
        Usage: D3D11_USAGE_DYNAMIC,
        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
        ..Default::default()
    };
    unsafe {
        let mut buffer = None;
        device.CreateBuffer(&desc, None, Some(&mut buffer))?;
        buffer.context("CreateBuffer returned null")
    }
}

fn create_glyph_input_layout(
    device: &ID3D11Device,
    shader_bytes: &[u8],
) -> Result<ID3D11InputLayout> {
    let elements = [
        D3D11_INPUT_ELEMENT_DESC {
            SemanticName: s!("SV_Position"),
            SemanticIndex: 0,
            Format: DXGI_FORMAT_R32G32_FLOAT,
            InputSlot: 0,
            AlignedByteOffset: 0,
            InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
            InstanceDataStepRate: 0,
        },
        D3D11_INPUT_ELEMENT_DESC {
            SemanticName: s!("shadingType"),
            SemanticIndex: 0,
            Format: DXGI_FORMAT_R16_UINT,
            InputSlot: 1,
            AlignedByteOffset: 0,
            InputSlotClass: D3D11_INPUT_PER_INSTANCE_DATA,
            InstanceDataStepRate: 1,
        },
        D3D11_INPUT_ELEMENT_DESC {
            SemanticName: s!("renditionScale"),
            SemanticIndex: 0,
            Format: DXGI_FORMAT_R8G8_UINT,
            InputSlot: 1,
            AlignedByteOffset: 2,
            InputSlotClass: D3D11_INPUT_PER_INSTANCE_DATA,
            InstanceDataStepRate: 1,
        },
        D3D11_INPUT_ELEMENT_DESC {
            SemanticName: s!("position"),
            SemanticIndex: 0,
            Format: DXGI_FORMAT_R16G16_SINT,
            InputSlot: 1,
            AlignedByteOffset: 4,
            InputSlotClass: D3D11_INPUT_PER_INSTANCE_DATA,
            InstanceDataStepRate: 1,
        },
        D3D11_INPUT_ELEMENT_DESC {
            SemanticName: s!("size"),
            SemanticIndex: 0,
            Format: DXGI_FORMAT_R16G16_UINT,
            InputSlot: 1,
            AlignedByteOffset: 8,
            InputSlotClass: D3D11_INPUT_PER_INSTANCE_DATA,
            InstanceDataStepRate: 1,
        },
        D3D11_INPUT_ELEMENT_DESC {
            SemanticName: s!("texcoord"),
            SemanticIndex: 0,
            Format: DXGI_FORMAT_R16G16_UINT,
            InputSlot: 1,
            AlignedByteOffset: 12,
            InputSlotClass: D3D11_INPUT_PER_INSTANCE_DATA,
            InstanceDataStepRate: 1,
        },
        D3D11_INPUT_ELEMENT_DESC {
            SemanticName: s!("color"),
            SemanticIndex: 0,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            InputSlot: 1,
            AlignedByteOffset: 16,
            InputSlotClass: D3D11_INPUT_PER_INSTANCE_DATA,
            InstanceDataStepRate: 1,
        },
    ];

    unsafe {
        let mut layout = None;
        device.CreateInputLayout(&elements, shader_bytes, Some(&mut layout))?;
        layout.context("CreateInputLayout returned null")
    }
}

fn create_static_vertex_buffer<T: Pod>(device: &ID3D11Device, data: &[T]) -> Result<ID3D11Buffer> {
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: size_of_val(data) as u32,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
        CPUAccessFlags: 0,
        ..Default::default()
    };
    let initial_data = D3D11_SUBRESOURCE_DATA {
        pSysMem: data.as_ptr() as *const _,
        ..Default::default()
    };

    unsafe {
        let mut buffer = None;
        device.CreateBuffer(&desc, Some(&initial_data), Some(&mut buffer))?;
        buffer.context("CreateBuffer returned null")
    }
}

fn create_static_index_buffer(device: &ID3D11Device, data: &[u16]) -> Result<ID3D11Buffer> {
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: size_of_val(data) as u32,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_INDEX_BUFFER.0 as u32,
        CPUAccessFlags: 0,
        ..Default::default()
    };
    let initial_data = D3D11_SUBRESOURCE_DATA {
        pSysMem: data.as_ptr() as *const _,
        ..Default::default()
    };

    unsafe {
        let mut buffer = None;
        device.CreateBuffer(&desc, Some(&initial_data), Some(&mut buffer))?;
        buffer.context("CreateBuffer returned null")
    }
}

fn create_dynamic_vertex_buffer<T>(
    device: &ID3D11Device,
    element_count: usize,
) -> Result<ID3D11Buffer> {
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: (size_of::<T>() * element_count.max(1)) as u32,
        Usage: D3D11_USAGE_DYNAMIC,
        BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
        ..Default::default()
    };

    unsafe {
        let mut output = None;
        device.CreateBuffer(&desc, None, Some(&mut output))?;
        output.context("CreateBuffer returned null")
    }
}

fn write_buffer(context: &ID3D11DeviceContext, buffer: &ID3D11Buffer, bytes: &[u8]) -> Result<()> {
    unsafe {
        let mut mapped = std::mem::zeroed();
        context.Map(buffer, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.pData as *mut u8, bytes.len());
        context.Unmap(buffer, 0);
    }
    Ok(())
}
