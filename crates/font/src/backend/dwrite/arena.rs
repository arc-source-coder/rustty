use std::sync::OnceLock;

use anyhow::{Result, anyhow};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_GLYPH_OFFSET, DWRITE_SHAPING_GLYPH_PROPERTIES, DWRITE_SHAPING_TEXT_PROPERTIES,
};
use windows::Win32::System::Memory::{
    MEM_COMMIT, MEM_DECOMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
};
use windows::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};

use crate::types::{BidiRun, Cell, GlyphOffset, RunSpan, ScriptRun, TextRun};

/// VirtualAlloc-backed contiguous byte buffer.
struct VmBuffer {
    base: *mut u8,
    reserved: usize,
    committed: usize,
}

#[derive(Clone, Copy, Debug)]
struct VmGranularity {
    reserve: usize,
    commit: usize,
}

fn vm_granularity() -> VmGranularity {
    static GRAN: OnceLock<VmGranularity> = OnceLock::new();
    *GRAN.get_or_init(|| {
        // SAFETY: `GetSystemInfo` writes to initialized out-parameter.
        let info = unsafe {
            let mut v = SYSTEM_INFO::default();
            GetSystemInfo(&mut v);
            v
        };
        VmGranularity {
            reserve: (info.dwAllocationGranularity as usize).max(4096),
            commit: (info.dwPageSize as usize).max(4096),
        }
    })
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArenaResetMode {
    RetainCommitted,
    Decommit,
    Release,
}

impl VmBuffer {
    const MIN_RESERVE: usize = 1024 * 1024;

    fn new() -> Self {
        Self {
            base: std::ptr::null_mut(),
            reserved: 0,
            committed: 0,
        }
    }

    fn ensure_len(&mut self, required_len: usize) -> Result<()> {
        if required_len <= self.committed {
            return Ok(());
        }
        self.ensure_reserved(required_len)?;
        let gran = vm_granularity();
        let target = align_up_to(required_len, gran.commit);
        if target > self.committed {
            let commit_addr = unsafe { self.base.add(self.committed) };
            let commit_size = target - self.committed;
            // SAFETY: `commit_addr` points inside a region reserved by this VmBuffer.
            let committed = unsafe {
                VirtualAlloc(
                    Some(commit_addr.cast::<core::ffi::c_void>()),
                    commit_size,
                    MEM_COMMIT,
                    PAGE_READWRITE,
                )
            };
            if committed.is_null() {
                return Err(anyhow!("VirtualAlloc MEM_COMMIT failed"));
            }
            self.committed = target;
        }
        Ok(())
    }

    /// Ensures committed length but allows dropping existing content.
    ///
    /// Used for scratch arenas where old bytes are never needed after growth.
    fn ensure_len_discard(&mut self, required_len: usize) -> Result<()> {
        if required_len <= self.committed {
            return Ok(());
        }
        let gran = vm_granularity();
        if required_len > self.reserved {
            let mut new_reserved = if self.reserved == 0 {
                Self::MIN_RESERVE
            } else {
                self.reserved.saturating_mul(2)
            };
            if new_reserved < required_len {
                new_reserved = required_len;
            }
            new_reserved = align_up_to(new_reserved, gran.reserve);
            self.release();
            self.reserve(new_reserved)?;
        }
        let target = align_up_to(required_len, gran.commit);
        if target > self.committed {
            let commit_addr = unsafe { self.base.add(self.committed) };
            let commit_size = target - self.committed;
            // SAFETY: `commit_addr` points inside a region reserved by this VmBuffer.
            let committed = unsafe {
                VirtualAlloc(
                    Some(commit_addr.cast::<core::ffi::c_void>()),
                    commit_size,
                    MEM_COMMIT,
                    PAGE_READWRITE,
                )
            };
            if committed.is_null() {
                return Err(anyhow!("VirtualAlloc MEM_COMMIT failed"));
            }
            self.committed = target;
        }
        Ok(())
    }

    fn ensure_reserved(&mut self, required_len: usize) -> Result<()> {
        if required_len <= self.reserved {
            return Ok(());
        }
        let gran = vm_granularity();

        let mut new_reserved = if self.reserved == 0 {
            Self::MIN_RESERVE
        } else {
            self.reserved.saturating_mul(2)
        };
        if new_reserved < required_len {
            new_reserved = required_len;
        }
        new_reserved = align_up_to(new_reserved, gran.reserve);

        let mut new_buf = VmBuffer::new();
        new_buf.reserve(new_reserved)?;
        if self.committed > 0 {
            new_buf.ensure_len(self.committed)?;
            // SAFETY: both buffers are committed for at least `self.committed` bytes.
            unsafe {
                std::ptr::copy_nonoverlapping(self.base, new_buf.base, self.committed);
            }
        }
        self.release();
        *self = new_buf;
        Ok(())
    }

    fn reserve(&mut self, reserve_len: usize) -> Result<()> {
        let reserve_len = align_up_to(reserve_len, vm_granularity().reserve);
        // SAFETY: requesting a fresh reserved region from OS.
        let ptr = unsafe { VirtualAlloc(None, reserve_len, MEM_RESERVE, PAGE_READWRITE) };
        if ptr.is_null() {
            return Err(anyhow!("VirtualAlloc MEM_RESERVE failed"));
        }
        self.base = ptr.cast::<u8>();
        self.reserved = reserve_len;
        self.committed = 0;
        Ok(())
    }

    #[inline]
    fn as_ptr(&self) -> *const u8 {
        self.base.cast::<u8>()
    }

    #[inline]
    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.base.cast::<u8>()
    }

    fn release(&mut self) {
        if self.base.is_null() {
            return;
        }
        // SAFETY: base was returned by VirtualAlloc and is owned by this buffer.
        let _ = unsafe { VirtualFree(self.base.cast::<core::ffi::c_void>(), 0, MEM_RELEASE) };
        self.base = std::ptr::null_mut();
        self.reserved = 0;
        self.committed = 0;
    }

    #[allow(dead_code)]
    fn reset(&mut self, mode: ArenaResetMode) {
        match mode {
            ArenaResetMode::RetainCommitted => {}
            ArenaResetMode::Decommit => {
                if self.base.is_null() || self.committed == 0 {
                    return;
                }
                // SAFETY: base is valid VirtualAlloc memory; MEM_DECOMMIT expects committed bytes.
                let _ = unsafe {
                    VirtualFree(
                        self.base.cast::<core::ffi::c_void>(),
                        self.committed,
                        MEM_DECOMMIT,
                    )
                };
                self.committed = 0;
            }
            ArenaResetMode::Release => self.release(),
        }
    }
}

impl Drop for VmBuffer {
    fn drop(&mut self) {
        self.release();
    }
}

/// Capacity targets for the persistent output arena.
///
/// We keep these as logical element counts (not bytes) so call sites can reason
/// in domain terms (`runs`, `glyphs`, `cells`) and the layout builder handles
/// byte sizing/alignment.
#[derive(Clone, Copy, Default)]
pub(crate) struct OutputCaps {
    pub runs: usize,
    pub spans: usize,
    pub cells: usize,
    pub cluster: usize,
    pub glyph: usize,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct OutputUsed {
    pub runs: usize,
    pub spans: usize,
    pub cells: usize,
    pub cluster: usize,
    pub glyph: usize,
}

#[derive(Clone, Copy, Default)]
struct OutputLayout {
    runs: usize,
    run_cell_spans: usize,
    run_glyph_spans: usize,
    run_cluster_spans: usize,
    cells: usize,
    cluster_map: usize,
    glyph_advances: usize,
    glyph_offsets: usize,
    total: usize,
}

/// Persistent output arena for shape results returned to renderer.
///
/// Why arena here:
/// - keeps all output SoA arrays in one contiguous allocation family,
/// - reduces independent growth/reallocation churn,
/// - improves cache locality during emit/flatten paths.
pub(crate) struct OutputArena {
    buf: VmBuffer,
    caps: OutputCaps,
    layout: OutputLayout,
}

impl OutputArena {
    pub(crate) fn new() -> Self {
        Self {
            buf: VmBuffer::new(),
            caps: OutputCaps::default(),
            layout: OutputLayout::default(),
        }
    }

    /// Ensures output capacities, preserving currently-used prefix data.
    ///
    /// Existing shaped data is copied only when growth is required.
    pub(crate) fn ensure(&mut self, required: OutputCaps, used: OutputUsed) -> Result<()> {
        let next = OutputCaps {
            runs: grow_cap(self.caps.runs, required.runs),
            spans: grow_cap(self.caps.spans, required.spans),
            cells: grow_cap(self.caps.cells, required.cells),
            cluster: grow_cap(self.caps.cluster, required.cluster),
            glyph: grow_cap(self.caps.glyph, required.glyph),
        };
        if next.runs == self.caps.runs
            && next.spans == self.caps.spans
            && next.cells == self.caps.cells
            && next.cluster == self.caps.cluster
            && next.glyph == self.caps.glyph
        {
            return Ok(());
        }

        let old_layout = self.layout;
        let new_layout = output_layout(next);
        let mut new_buf = VmBuffer::new();
        new_buf.ensure_len(new_layout.total)?;

        if !self.buf.base.is_null() {
            // SAFETY: copy ranges are bounded by old used lengths and destination capacities.
            unsafe {
                copy_typed_region::<TextRun>(
                    self.buf.as_ptr(),
                    old_layout.runs,
                    new_buf.as_mut_ptr(),
                    new_layout.runs,
                    used.runs,
                );
                copy_typed_region::<RunSpan>(
                    self.buf.as_ptr(),
                    old_layout.run_cell_spans,
                    new_buf.as_mut_ptr(),
                    new_layout.run_cell_spans,
                    used.spans,
                );
                copy_typed_region::<RunSpan>(
                    self.buf.as_ptr(),
                    old_layout.run_glyph_spans,
                    new_buf.as_mut_ptr(),
                    new_layout.run_glyph_spans,
                    used.spans,
                );
                copy_typed_region::<RunSpan>(
                    self.buf.as_ptr(),
                    old_layout.run_cluster_spans,
                    new_buf.as_mut_ptr(),
                    new_layout.run_cluster_spans,
                    used.spans,
                );
                copy_typed_region::<Cell>(
                    self.buf.as_ptr(),
                    old_layout.cells,
                    new_buf.as_mut_ptr(),
                    new_layout.cells,
                    used.cells,
                );
                copy_typed_region::<u16>(
                    self.buf.as_ptr(),
                    old_layout.cluster_map,
                    new_buf.as_mut_ptr(),
                    new_layout.cluster_map,
                    used.cluster,
                );
                copy_typed_region::<f32>(
                    self.buf.as_ptr(),
                    old_layout.glyph_advances,
                    new_buf.as_mut_ptr(),
                    new_layout.glyph_advances,
                    used.glyph,
                );
                copy_typed_region::<GlyphOffset>(
                    self.buf.as_ptr(),
                    old_layout.glyph_offsets,
                    new_buf.as_mut_ptr(),
                    new_layout.glyph_offsets,
                    used.glyph,
                );
            }
        }

        self.buf = new_buf;
        self.caps = next;
        self.layout = new_layout;
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn reset(&mut self, mode: ArenaResetMode) {
        self.buf.reset(mode);
        if mode != ArenaResetMode::RetainCommitted {
            self.caps = OutputCaps::default();
            self.layout = OutputLayout::default();
        }
    }

    #[inline]
    /// Returns a typed mutable pointer into the contiguous output arena block.
    ///
    /// SAFETY: `offset` must target a region aligned for `T` and fully inside
    /// the arena allocation for the intended subsequent accesses.
    unsafe fn ptr_mut<T>(&mut self, offset: usize) -> *mut T {
        // SAFETY: caller upholds alignment and bounds contract for this typed view.
        unsafe { self.buf.as_mut_ptr().add(offset).cast::<T>() }
    }

    #[inline]
    /// Returns a typed const pointer into the contiguous output arena block.
    ///
    /// SAFETY: `offset` must target a region aligned for `T` and fully inside
    /// the arena allocation for the intended subsequent accesses.
    unsafe fn ptr<T>(&self, offset: usize) -> *const T {
        // SAFETY: caller upholds alignment and bounds contract for this typed view.
        unsafe { self.buf.as_ptr().add(offset).cast::<T>() }
    }

    #[inline]
    pub(crate) unsafe fn runs_mut_ptr(&mut self) -> *mut TextRun {
        unsafe { self.ptr_mut(self.layout.runs) }
    }
    #[inline]
    pub(crate) unsafe fn runs_ptr(&self) -> *const TextRun {
        unsafe { self.ptr(self.layout.runs) }
    }
    #[inline]
    pub(crate) unsafe fn run_cell_spans_mut_ptr(&mut self) -> *mut RunSpan {
        unsafe { self.ptr_mut(self.layout.run_cell_spans) }
    }
    #[inline]
    pub(crate) unsafe fn run_cell_spans_ptr(&self) -> *const RunSpan {
        unsafe { self.ptr(self.layout.run_cell_spans) }
    }
    #[inline]
    pub(crate) unsafe fn run_glyph_spans_mut_ptr(&mut self) -> *mut RunSpan {
        unsafe { self.ptr_mut(self.layout.run_glyph_spans) }
    }
    #[inline]
    pub(crate) unsafe fn run_glyph_spans_ptr(&self) -> *const RunSpan {
        unsafe { self.ptr(self.layout.run_glyph_spans) }
    }
    #[inline]
    pub(crate) unsafe fn run_cluster_spans_mut_ptr(&mut self) -> *mut RunSpan {
        unsafe { self.ptr_mut(self.layout.run_cluster_spans) }
    }
    #[inline]
    pub(crate) unsafe fn run_cluster_spans_ptr(&self) -> *const RunSpan {
        unsafe { self.ptr(self.layout.run_cluster_spans) }
    }
    #[inline]
    pub(crate) unsafe fn cells_mut_ptr(&mut self) -> *mut Cell {
        unsafe { self.ptr_mut(self.layout.cells) }
    }
    #[inline]
    pub(crate) unsafe fn cells_ptr(&self) -> *const Cell {
        unsafe { self.ptr(self.layout.cells) }
    }
    #[inline]
    pub(crate) unsafe fn cluster_map_mut_ptr(&mut self) -> *mut u16 {
        unsafe { self.ptr_mut(self.layout.cluster_map) }
    }
    #[inline]
    pub(crate) unsafe fn cluster_map_ptr(&self) -> *const u16 {
        unsafe { self.ptr(self.layout.cluster_map) }
    }
    #[inline]
    pub(crate) unsafe fn glyph_advances_mut_ptr(&mut self) -> *mut f32 {
        unsafe { self.ptr_mut(self.layout.glyph_advances) }
    }
    #[inline]
    pub(crate) unsafe fn glyph_advances_ptr(&self) -> *const f32 {
        unsafe { self.ptr(self.layout.glyph_advances) }
    }
    #[inline]
    pub(crate) unsafe fn glyph_offsets_mut_ptr(&mut self) -> *mut GlyphOffset {
        unsafe { self.ptr_mut(self.layout.glyph_offsets) }
    }
    #[inline]
    pub(crate) unsafe fn glyph_offsets_ptr(&self) -> *const GlyphOffset {
        unsafe { self.ptr(self.layout.glyph_offsets) }
    }
}

fn output_layout(c: OutputCaps) -> OutputLayout {
    // Layout is explicitly SoA + aligned offsets so typed pointer views can be
    // materialized with no per-element indirection.
    let mut at = 0usize;

    at = align_up(at, std::mem::align_of::<TextRun>());
    let runs = at;
    at += c.runs * std::mem::size_of::<TextRun>();

    at = align_up(at, std::mem::align_of::<RunSpan>());
    let run_cell_spans = at;
    at += c.spans * std::mem::size_of::<RunSpan>();

    at = align_up(at, std::mem::align_of::<RunSpan>());
    let run_glyph_spans = at;
    at += c.spans * std::mem::size_of::<RunSpan>();

    at = align_up(at, std::mem::align_of::<RunSpan>());
    let run_cluster_spans = at;
    at += c.spans * std::mem::size_of::<RunSpan>();

    at = align_up(at, std::mem::align_of::<Cell>());
    let cells = at;
    at += c.cells * std::mem::size_of::<Cell>();

    at = align_up(at, std::mem::align_of::<u16>());
    let cluster_map = at;
    at += c.cluster * std::mem::size_of::<u16>();

    at = align_up(at, std::mem::align_of::<f32>());
    let glyph_advances = at;
    at += c.glyph * std::mem::size_of::<f32>();

    at = align_up(at, std::mem::align_of::<GlyphOffset>());
    let glyph_offsets = at;
    at += c.glyph * std::mem::size_of::<GlyphOffset>();

    OutputLayout {
        runs,
        run_cell_spans,
        run_glyph_spans,
        run_cluster_spans,
        cells,
        cluster_map,
        glyph_advances,
        glyph_offsets,
        total: align_up(at, std::mem::align_of::<u64>()),
    }
}

/// Capacity targets for temporary per-shape DirectWrite scratch storage.
///
/// `script_runs` holds `AnalyzeScript` output and lets us remove the extra
/// side `Vec<ScriptRun>` in the analyzer.
#[derive(Clone, Copy, Default)]
pub(crate) struct ScratchCaps {
    pub script_runs: usize,
    pub bidi_runs: usize,
    pub text: usize,
    pub glyph: usize,
}

#[derive(Clone, Copy, Default)]
struct ScratchLayout {
    script_runs: usize,
    bidi_runs: usize,
    cluster_map: usize,
    text_props: usize,
    glyph_indices: usize,
    glyph_props: usize,
    glyph_advances: usize,
    glyph_offsets: usize,
    total: usize,
}

/// Per-shape scratch arena for DirectWrite API temporary buffers.
///
/// This includes script-analysis output and glyph-placement intermediates.
/// The contents are transient and rewritten each shape() call.
pub(crate) struct ScratchArena {
    buf: VmBuffer,
    caps: ScratchCaps,
    layout: ScratchLayout,
}

impl ScratchArena {
    pub(crate) fn new() -> Self {
        Self {
            buf: VmBuffer::new(),
            caps: ScratchCaps::default(),
            layout: ScratchLayout::default(),
        }
    }

    /// Ensures scratch capacities for the next shape segment/call.
    ///
    /// Scratch contents are intentionally discarded on growth because they are
    /// temporary and never part of returned API data.
    pub(crate) fn ensure(&mut self, required: ScratchCaps) -> Result<()> {
        let next = ScratchCaps {
            script_runs: grow_cap(self.caps.script_runs, required.script_runs),
            bidi_runs: grow_cap(self.caps.bidi_runs, required.bidi_runs),
            text: grow_cap(self.caps.text, required.text),
            glyph: grow_cap(self.caps.glyph, required.glyph),
        };
        if next.script_runs == self.caps.script_runs
            && next.bidi_runs == self.caps.bidi_runs
            && next.text == self.caps.text
            && next.glyph == self.caps.glyph
        {
            return Ok(());
        }

        self.layout = scratch_layout(next);
        self.buf.ensure_len_discard(self.layout.total)?;
        self.caps = next;
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn reset(&mut self, mode: ArenaResetMode) {
        self.buf.reset(mode);
        if mode != ArenaResetMode::RetainCommitted {
            self.caps = ScratchCaps::default();
            self.layout = ScratchLayout::default();
        }
    }

    #[inline]
    pub(crate) fn glyph_cap(&self) -> usize {
        self.caps.glyph
    }

    #[inline]
    pub(crate) fn script_runs_cap(&self) -> usize {
        self.caps.script_runs
    }

    #[inline]
    pub(crate) fn bidi_runs_cap(&self) -> usize {
        self.caps.bidi_runs
    }

    #[inline]
    /// Returns a typed mutable pointer into scratch storage.
    ///
    /// SAFETY: `offset` must be aligned for `T` and point into this scratch
    /// allocation for all accesses performed through the returned pointer.
    unsafe fn ptr_mut<T>(&mut self, offset: usize) -> *mut T {
        // SAFETY: caller enforces alignment and bounds for this typed view.
        unsafe { self.buf.as_mut_ptr().add(offset).cast::<T>() }
    }

    #[inline]
    /// Returns a typed const pointer into scratch storage.
    ///
    /// SAFETY: `offset` must be aligned for `T` and point into this scratch
    /// allocation for all accesses performed through the returned pointer.
    unsafe fn ptr<T>(&self, offset: usize) -> *const T {
        // SAFETY: caller enforces alignment and bounds for this typed view.
        unsafe { self.buf.as_ptr().add(offset).cast::<T>() }
    }

    #[inline]
    pub(crate) unsafe fn script_runs_mut_ptr(&mut self) -> *mut ScriptRun {
        // SAFETY: layout places `script_runs` at a ScriptRun-aligned offset.
        unsafe { self.ptr_mut(self.layout.script_runs) }
    }
    #[inline]
    pub(crate) unsafe fn script_runs_ptr(&self) -> *const ScriptRun {
        // SAFETY: layout places `script_runs` at a ScriptRun-aligned offset.
        unsafe { self.ptr(self.layout.script_runs) }
    }
    #[inline]
    pub(crate) unsafe fn bidi_runs_mut_ptr(&mut self) -> *mut BidiRun {
        // SAFETY: layout places `bidi_runs` at a BidiRun-aligned offset.
        unsafe { self.ptr_mut(self.layout.bidi_runs) }
    }
    #[inline]
    pub(crate) unsafe fn bidi_runs_ptr(&self) -> *const BidiRun {
        // SAFETY: layout places `bidi_runs` at a BidiRun-aligned offset.
        unsafe { self.ptr(self.layout.bidi_runs) }
    }
    #[inline]
    pub(crate) unsafe fn cluster_map_mut_ptr(&mut self) -> *mut u16 {
        unsafe { self.ptr_mut(self.layout.cluster_map) }
    }
    #[inline]
    pub(crate) unsafe fn cluster_map_ptr(&self) -> *const u16 {
        unsafe { self.ptr(self.layout.cluster_map) }
    }
    #[inline]
    pub(crate) unsafe fn text_props_mut_ptr(&mut self) -> *mut DWRITE_SHAPING_TEXT_PROPERTIES {
        unsafe { self.ptr_mut(self.layout.text_props) }
    }
    #[inline]
    pub(crate) unsafe fn glyph_indices_mut_ptr(&mut self) -> *mut u16 {
        unsafe { self.ptr_mut(self.layout.glyph_indices) }
    }
    #[inline]
    pub(crate) unsafe fn glyph_indices_ptr(&self) -> *const u16 {
        unsafe { self.ptr(self.layout.glyph_indices) }
    }
    #[inline]
    pub(crate) unsafe fn glyph_props_mut_ptr(&mut self) -> *mut DWRITE_SHAPING_GLYPH_PROPERTIES {
        unsafe { self.ptr_mut(self.layout.glyph_props) }
    }
    #[inline]
    pub(crate) unsafe fn glyph_props_ptr(&self) -> *const DWRITE_SHAPING_GLYPH_PROPERTIES {
        unsafe { self.ptr(self.layout.glyph_props) }
    }
    #[inline]
    pub(crate) unsafe fn glyph_advances_mut_ptr(&mut self) -> *mut f32 {
        unsafe { self.ptr_mut(self.layout.glyph_advances) }
    }
    #[inline]
    pub(crate) unsafe fn glyph_advances_ptr(&self) -> *const f32 {
        unsafe { self.ptr(self.layout.glyph_advances) }
    }
    #[inline]
    pub(crate) unsafe fn glyph_offsets_mut_ptr(&mut self) -> *mut GlyphOffset {
        unsafe { self.ptr_mut(self.layout.glyph_offsets) }
    }
    #[inline]
    pub(crate) unsafe fn glyph_offsets_ptr(&self) -> *const GlyphOffset {
        unsafe { self.ptr(self.layout.glyph_offsets) }
    }
}

fn scratch_layout(c: ScratchCaps) -> ScratchLayout {
    // Scratch layout is contiguous and alignment-aware; all DWrite input/output
    // pointers are derived from these deterministic offsets.
    let mut at = 0usize;

    at = align_up(at, std::mem::align_of::<ScriptRun>());
    let script_runs = at;
    at += c.script_runs * std::mem::size_of::<ScriptRun>();

    at = align_up(at, std::mem::align_of::<BidiRun>());
    let bidi_runs = at;
    at += c.bidi_runs * std::mem::size_of::<BidiRun>();

    at = align_up(at, std::mem::align_of::<u16>());
    let cluster_map = at;
    at += (c.text + 1) * std::mem::size_of::<u16>();

    at = align_up(at, std::mem::align_of::<DWRITE_SHAPING_TEXT_PROPERTIES>());
    let text_props = at;
    at += c.text * std::mem::size_of::<DWRITE_SHAPING_TEXT_PROPERTIES>();

    at = align_up(at, std::mem::align_of::<u16>());
    let glyph_indices = at;
    at += c.glyph * std::mem::size_of::<u16>();

    at = align_up(at, std::mem::align_of::<DWRITE_SHAPING_GLYPH_PROPERTIES>());
    let glyph_props = at;
    at += c.glyph * std::mem::size_of::<DWRITE_SHAPING_GLYPH_PROPERTIES>();

    at = align_up(at, std::mem::align_of::<f32>());
    let glyph_advances = at;
    at += c.glyph * std::mem::size_of::<f32>();

    at = align_up(at, std::mem::align_of::<GlyphOffset>());
    let glyph_offsets = at;
    at += c.glyph * std::mem::size_of::<GlyphOffset>();

    ScratchLayout {
        script_runs,
        bidi_runs,
        cluster_map,
        text_props,
        glyph_indices,
        glyph_props,
        glyph_advances,
        glyph_offsets,
        total: align_up(at, std::mem::align_of::<u64>()),
    }
}

#[inline]
fn align_up(v: usize, align: usize) -> usize {
    align_up_to(v, align)
}

#[inline]
fn align_up_to(v: usize, align: usize) -> usize {
    debug_assert!(align > 0);
    if align.is_power_of_two() {
        (v + (align - 1)) & !(align - 1)
    } else {
        v.div_ceil(align) * align
    }
}

#[inline]
fn grow_cap(current: usize, required: usize) -> usize {
    if current >= required {
        current
    } else {
        required.max(current.saturating_add(current / 2).saturating_add(16))
    }
}

#[inline]
unsafe fn copy_typed_region<T>(
    src_base: *const u8,
    src_off: usize,
    dst_base: *mut u8,
    dst_off: usize,
    count: usize,
) {
    // SAFETY: caller guarantees offsets are aligned for T and ranges are valid.
    unsafe {
        std::ptr::copy_nonoverlapping(
            src_base.add(src_off).cast::<T>(),
            dst_base.add(dst_off).cast::<T>(),
            count,
        );
    }
}

const _: [(); std::mem::size_of::<GlyphOffset>()] =
    [(); std::mem::size_of::<DWRITE_GLYPH_OFFSET>()];
const _: [(); std::mem::align_of::<GlyphOffset>()] =
    [(); std::mem::align_of::<DWRITE_GLYPH_OFFSET>()];
const _: [(); 1] = [(); (std::mem::align_of::<TextRun>() <= std::mem::align_of::<u64>()) as usize];
const _: [(); 1] = [(); (std::mem::align_of::<RunSpan>() <= std::mem::align_of::<u64>()) as usize];
const _: [(); 1] = [(); (std::mem::align_of::<Cell>() <= std::mem::align_of::<u64>()) as usize];
const _: [(); 1] =
    [(); (std::mem::align_of::<ScriptRun>() <= std::mem::align_of::<u64>()) as usize];
const _: [(); 1] = [(); (std::mem::align_of::<BidiRun>() <= std::mem::align_of::<u64>()) as usize];
const _: [(); 1] =
    [(); (std::mem::align_of::<GlyphOffset>() <= std::mem::align_of::<u64>()) as usize];
const _: [(); 1] = [(); (std::mem::align_of::<DWRITE_SHAPING_TEXT_PROPERTIES>()
    <= std::mem::align_of::<u64>()) as usize];
const _: [(); 1] = [(); (std::mem::align_of::<DWRITE_SHAPING_GLYPH_PROPERTIES>()
    <= std::mem::align_of::<u64>()) as usize];
