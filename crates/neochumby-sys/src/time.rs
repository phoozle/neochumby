//! Sleeping and the local time.

/// Sleeps about `ms` milliseconds, as poll(2) with no descriptors. On the
/// Chumby's kernel, std::thread::sleep panics when an interrupted nanosleep
/// fails with EINVAL; this returns early instead.
pub fn sleep_ms(ms: u32) {
    let ms = ms.min(i32::MAX as u32) as libc::c_int;
    // SAFETY: with no descriptors, poll(2) reads nothing through its null
    // pointer; it only waits.
    unsafe { libc::poll(std::ptr::null_mut(), 0, ms) };
}

/// The wall clock in the local time zone, from the C library's time zone
/// (TZ).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    /// 0-23.
    pub hour: u8,
    /// 0-59.
    pub minute: u8,
    /// 0-60 (a leap second).
    pub second: u8,
}

/// The time now (time(2), then localtime_r(3)); midnight if the C library
/// can't convert it.
pub fn local_time() -> LocalTime {
    // SAFETY: time(2) with a null pointer only returns the time.
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    local(now)
}

/// The local time at `t`, seconds since the epoch (localtime_r(3));
/// midnight if the C library can't convert it.
// libc marks 32-bit musl's time_t deprecated (it becomes 64-bit with musl
// 1.2); either width holds the time until 2038.
#[allow(deprecated)]
pub fn local_time_at(t: i64) -> LocalTime {
    local(t as libc::time_t)
}

#[allow(deprecated)]
fn local(now: libc::time_t) -> LocalTime {
    // SAFETY: libc::tm is integers and, on some systems, a pointer to the
    // zone's name, for which null is a valid value.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are to live locals of the right types;
    // localtime_r writes only `tm`, and on failure leaves it as it was.
    unsafe { libc::localtime_r(&now, &mut tm) };
    let field = |v: libc::c_int| v.clamp(0, 60) as u8;
    LocalTime { hour: field(tm.tm_hour), minute: field(tm.tm_min), second: field(tm.tm_sec) }
}
