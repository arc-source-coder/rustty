// TODO: Doc/module comments

#[inline(always)]
pub const fn assert(ok: bool) {
    debug_assert!(ok);
    unsafe { std::hint::assert_unchecked(ok) };
}

#[inline(always)]
pub const fn unreachable() -> ! {
    debug_assert!(false);
    unsafe { std::hint::unreachable_unchecked() }
}
