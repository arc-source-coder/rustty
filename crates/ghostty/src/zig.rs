use utils::asserts::assert;

use crate::ffi::{CellStyle, OptionalSelection, U21};
use crate::types::RawCell;
use std::marker::PhantomData;
use std::mem::MaybeUninit;

/// Layout of Zig `std.MultiArrayList(T)` on the supported Ghostty shim target.
///
/// Header fields are `{ bytes: [*]u8, len: usize, capacity: usize }`.
/// Column pointers are computed as:
///
/// `field_ptr = bytes + FIELD_PREFIX_SIZE * capacity`
///
/// The returned slice length is always `len`.
pub const MULTI_ARRAY_LIST_SIZE: usize = 24;
pub const MULTI_ARRAY_LIST_ALIGN: usize = 8;

/// Prefix sizes for directly-read `std.MultiArrayList(terminal.RenderState.Row)` columns.
pub const ROW_MAL_CELLS_PREFIX_SIZE: usize = 48;
pub const ROW_MAL_SELECTION_PREFIX_SIZE: usize = 96;
pub const ROW_MAL_DIRTY_ROWS_PREFIX_SIZE: usize = 102;

/// Prefix sizes for directly-read `std.MultiArrayList(terminal.RenderState.Cell)` columns.
pub const CELL_MAL_RAW_CELLS_PREFIX_SIZE: usize = 0;
pub const CELL_MAL_GRAPHEME_PREFIX_SIZE: usize = 8;
pub const CELL_MAL_STYLE_PREFIX_SIZE: usize = 24;

/// FFI-compatible mirror of Zig `std.MultiArrayList(T)`.
///
/// The pointed-to bytes are owned by Zig. This header is only useful when paired
/// with type-specific prefix constants, such as `ROW_MAL_*_PREFIX_SIZE` or
/// `CELL_MAL_*_PREFIX_SIZE`.
#[repr(C)]
pub struct ZigMultiArrayList {
    bytes: MaybeUninit<*const u8>,
    len: usize,
    capacity: usize,
}

const _: () = assert!(size_of::<ZigMultiArrayList>() == MULTI_ARRAY_LIST_SIZE);
const _: () = assert!(align_of::<ZigMultiArrayList>() == MULTI_ARRAY_LIST_ALIGN);

impl ZigMultiArrayList {
    #[inline]
    const fn column<T>(&self, prefix_size: usize) -> &[T] {
        assert(self.len <= self.capacity);

        if self.len == 0 {
            return &[];
        }

        // len > 0 and len <= capacity implies capacity > 0, so Zig's
        // `bytes` field must contain an initialized allocation pointer.
        let base = unsafe { self.bytes.assume_init() };

        // Matches Zig MultiArrayList.slice(): column starts after all prior
        // field columns, each sized by capacity rather than len.
        let ptr = unsafe { base.add(prefix_size * self.capacity).cast::<T>() };

        unsafe { std::slice::from_raw_parts(ptr, self.len) }
    }
}

/// View into `std.MultiArrayList(terminal.RenderState.Row)`.
/// Caches the column pointers that Zig's `MultiArrayList.slice()` would compute.
pub struct RowView<'a> {
    dirty_rows: &'a [bool],
    selections: &'a [OptionalSelection],
    cell_multi_array_lists: &'a [ZigMultiArrayList],
}

impl<'a> From<&'a ZigMultiArrayList> for RowView<'a> {
    #[inline]
    fn from(mal: &'a ZigMultiArrayList) -> Self {
        Self {
            dirty_rows: mal.column(ROW_MAL_DIRTY_ROWS_PREFIX_SIZE),
            selections: mal.column(ROW_MAL_SELECTION_PREFIX_SIZE),
            cell_multi_array_lists: mal.column(ROW_MAL_CELLS_PREFIX_SIZE),
        }
    }
}

impl RowView<'_> {
    #[inline]
    pub const fn dirty_rows(&self) -> &[bool] {
        self.dirty_rows
    }

    #[inline]
    pub const fn selections(&self) -> &[OptionalSelection] {
        self.selections
    }

    #[inline]
    pub const fn cell_multi_array_lists(&self) -> &[ZigMultiArrayList] {
        self.cell_multi_array_lists
    }
}

/// View into `std.MultiArrayList(terminal.RenderState.Cell)`.
pub struct CellView<'a> {
    cells: &'a [RawCell],
    styles: &'a [MaybeUninit<CellStyle>],
    graphemes: &'a [MaybeUninit<GraphemeView<'a>>],
}

impl<'a> From<&'a ZigMultiArrayList> for CellView<'a> {
    #[inline]
    fn from(mal: &'a ZigMultiArrayList) -> Self {
        Self {
            cells: mal.column(CELL_MAL_RAW_CELLS_PREFIX_SIZE),
            styles: mal.column(CELL_MAL_STYLE_PREFIX_SIZE),
            graphemes: mal.column(CELL_MAL_GRAPHEME_PREFIX_SIZE),
        }
    }
}

impl CellView<'_> {
    #[inline]
    pub const fn raw_cells(&self) -> &[RawCell] {
        self.cells
    }

    #[inline]
    pub const fn styles(&self) -> &[MaybeUninit<CellStyle>] {
        self.styles
    }

    #[inline]
    pub const fn graphemes(&self) -> &[MaybeUninit<GraphemeView<'_>>] {
        self.graphemes
    }
}

/// FFI-compatible mirror of Zig `[]const u21`.
#[repr(C)]
pub struct GraphemeView<'a> {
    ptr: *const U21,
    len: usize,
    _lifetime: PhantomData<&'a [U21]>,
}

impl GraphemeView<'_> {
    /// Convert to a Rust slice, returning None if ptr is null or len is 0.
    ///
    /// # Safety
    ///
    /// Caller must verify that the corresponding raw cell is tagged `codepoint_grapheme`;
    /// Ghostty leaves this memory undefined for every other cell type.
    #[inline]
    pub const unsafe fn as_slice(&self) -> Option<&[U21]> {
        if self.ptr.is_null() || self.len == 0 {
            return None;
        }
        // SAFETY: CellView constructs GraphemeView with the lifetime of the
        // immutable render frame containing the grapheme allocation.
        Some(unsafe { std::slice::from_raw_parts(self.ptr, self.len) })
    }
}
