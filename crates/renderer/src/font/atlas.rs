use std::mem::ManuallyDrop;

use crate::atlas_allocator::{AtlasAllocator, AtlasFullError, Region};
use crate::backend::d3d11::GpuContext;
use anyhow::{Result, anyhow};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
    ID2D1Bitmap1, ID2D1DeviceContext4,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, ID3D11ShaderResourceView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_A8_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::IDXGISurface;
use windows::core::Interface as _;

/// Maximum atlas sizes. If a size larger than this is
/// needed, the atlas is cleared and new glyphs are re-rasterized lazily.
const MAX_GRAYSCALE_ATLAS_SIZE: u16 = 8192;
const MAX_COLOR_ATLAS_SIZE: u16 = 4096;

/// Pixel format of the atlas texture data.
/// Ghostty reference: `Atlas.Format`.
#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub enum AtlasFormat {
    /// 1 byte per pixel — grayscale text glyphs.
    #[default]
    Grayscale,
    /// 4 bytes per pixel — color emoji / color glyphs.
    Bgra,
}

pub struct Atlas {
    x_dpi: u16,
    y_dpi: u16,
    format: AtlasFormat,
    allocator: AtlasAllocator,
    pub resources: AtlasResources,
}

/// Effect of making room in an atlas after an allocation fails.
pub enum AtlasStatus {
    /// Texture replaced with a larger one; existing pixels and coordinates survive.
    Resized,
    /// Storage cleared; all glyph allocations in this format are invalid.
    Cleared(AtlasFormat),
}

impl Atlas {
    pub fn new(gpu: &GpuContext, ctx: &ID2D1DeviceContext4, options: AtlasOptions) -> Result<Self> {
        let mut resources = AtlasResources::new(gpu, ctx, options)?;
        resources.clear(ctx)?;
        Ok(Self {
            x_dpi: options.x_dpi,
            y_dpi: options.y_dpi,
            format: options.format,
            allocator: AtlasAllocator::new(options.side),
            resources,
        })
    }

    pub fn upload(&mut self, region: Region, pixels: &[u8], stride: u32) -> Result<()> {
        if region.width == 0 || region.height == 0 {
            return Ok(());
        }

        let destination = D2D_RECT_U {
            left: u32::from(region.x),
            top: u32::from(region.y),
            right: u32::from(region.x) + u32::from(region.width),
            bottom: u32::from(region.y) + u32::from(region.height),
        };

        unsafe {
            self.resources.bitmap.CopyFromMemory(
                Some(&raw const destination),
                pixels.as_ptr().cast(),
                stride,
            )?;
        }
        Ok(())
    }

    #[inline]
    pub fn reserve(&mut self, width: u16, height: u16) -> Result<Region, AtlasFullError> {
        self.allocator.reserve(width, height)
    }

    /// Double the atlas and preserve its pixels, or clear it at the size limit.
    /// Callers must end pending D2D drawing first and invalidate cached glyphs
    /// and retained quads when this returns `AtlasStatus::Cleared`.
    pub fn grow(&mut self, gpu: &GpuContext, ctx: &ID2D1DeviceContext4) -> Result<AtlasStatus> {
        let old_size = self.allocator.size;
        let new_size = old_size * 2;

        let max_atlas_size = match self.format {
            AtlasFormat::Grayscale => MAX_GRAYSCALE_ATLAS_SIZE,
            AtlasFormat::Bgra => MAX_COLOR_ATLAS_SIZE,
        };

        if new_size > max_atlas_size {
            self.resources.clear(ctx)?;
            self.allocator.clear();
            return Ok(AtlasStatus::Cleared(self.format));
        }

        let old_texture = self.resources.texture.clone();
        let options = AtlasOptions {
            side: new_size,
            x_dpi: self.x_dpi,
            y_dpi: self.y_dpi,
            format: self.format,
        };
        let mut new_resources = AtlasResources::new(gpu, ctx, options)?;
        new_resources.clear(ctx)?;

        let source_box = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: u32::from(old_size),
            bottom: u32::from(old_size),
            back: 1,
        };

        unsafe {
            gpu.context.CopySubresourceRegion(
                &new_resources.texture,
                0,
                0,
                0,
                0,
                &old_texture,
                0,
                Some(&raw const source_box),
            );
        }

        self.resources = new_resources;
        self.allocator.grow(new_size);
        Ok(AtlasStatus::Resized)
    }
}

#[derive(Clone, Copy)]
pub struct AtlasOptions {
    pub side: u16,
    pub x_dpi: u16,
    pub y_dpi: u16,
    pub format: AtlasFormat,
}

pub struct Atlases<'a> {
    pub grayscale: &'a AtlasResources,
    pub color: &'a AtlasResources,
}

pub struct AtlasResources {
    bitmap: ID2D1Bitmap1,
    texture: ID3D11Texture2D,
    pub srv: Option<ID3D11ShaderResourceView>,
}

impl AtlasResources {
    pub fn new(gpu: &GpuContext, ctx: &ID2D1DeviceContext4, options: AtlasOptions) -> Result<Self> {
        let dxgi_format = match options.format {
            AtlasFormat::Grayscale => DXGI_FORMAT_A8_UNORM,
            AtlasFormat::Bgra => DXGI_FORMAT_B8G8R8A8_UNORM,
        };

        let desc = D3D11_TEXTURE2D_DESC {
            Width: u32::from(options.side),
            Height: u32::from(options.side),
            MipLevels: 1,
            ArraySize: 1,
            Format: dxgi_format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32 | D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };

        let mut texture = None;
        unsafe { gpu.device.CreateTexture2D(&raw const desc, None, Some(&raw mut texture))? };
        // TODO: Replace with a typed error
        let texture = texture.ok_or_else(|| anyhow!("CreateTexture2D returned null"))?;

        let mut view = None;
        unsafe { gpu.device.CreateShaderResourceView(&texture, None, Some(&raw mut view))? };

        let surface: IDXGISurface = texture.cast()?;

        let properties = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: dxgi_format,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: f32::from(options.x_dpi),
            dpiY: f32::from(options.y_dpi),
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            colorContext: ManuallyDrop::new(None),
        };

        let bitmap: ID2D1Bitmap1 =
            unsafe { ctx.CreateBitmapFromDxgiSurface(&surface, Some(&raw const properties))? };

        Ok(AtlasResources { bitmap, texture, srv: view })
    }

    #[inline]
    pub fn bitmap(&self) -> &ID2D1Bitmap1 {
        &self.bitmap
    }

    #[inline]
    pub fn clear(&mut self, ctx: &ID2D1DeviceContext4) -> Result<()> {
        unsafe {
            ctx.BeginDraw();
            ctx.SetTarget(&self.bitmap);
            ctx.Clear(None);
            ctx.SetTarget(None);
            ctx.EndDraw(None, None)?;
        }
        Ok(())
    }
}
