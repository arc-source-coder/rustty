#[inline(always)]
pub const fn likely(b: bool) -> bool {
    if !b {
        std::hint::cold_path();
    }
    b
}

#[inline(always)]
pub const fn unlikely(b: bool) -> bool {
    if b {
        std::hint::cold_path();
    }
    b
}
