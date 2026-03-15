use font::shared_grid::SharedGrid;
use font::shared_grid_set::SharedGridPtr;

#[inline]
pub(crate) fn shared_grid_ref(ptr: SharedGridPtr) -> &'static SharedGrid {
    // SAFETY: pointers come from SharedGridSet and remain valid while the
    // renderer thread holds the corresponding grid ref.
    unsafe { &*ptr }
}
