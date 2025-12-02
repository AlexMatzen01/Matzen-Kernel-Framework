//! Compiler intrinsics required for no_std environments.
//!
//! These functions replace the standard C library implementations
//! of memcpy, memset, and memcmp that the Rust compiler expects.

use core::ffi::c_int;

#[no_mangle]
pub unsafe extern "C" fn memcpy(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    for i in 0..n {
        *dest.add(i) = *src.add(i);
    }
    dest
}

#[no_mangle]
pub unsafe extern "C" fn memset(s: *mut u8, c: c_int, n: usize) -> *mut u8 {
    let byte = c as u8;
    for i in 0..n {
        *s.add(i) = byte;
    }
    s
}

#[no_mangle]
pub unsafe extern "C" fn memcmp(s1: *const u8, s2: *const u8, n: usize) -> c_int {
    for i in 0..n {
        let a = *s1.add(i);
        let b = *s2.add(i);
        if a != b {
            return a as c_int - b as c_int;
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn memmove(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    if src < dest as *const u8 {
        // Copy backwards to handle overlapping regions
        let mut i = n;
        while i != 0 {
            i -= 1;
            *dest.add(i) = *src.add(i);
        }
    } else {
        // Copy forwards
        for i in 0..n {
            *dest.add(i) = *src.add(i);
        }
    }
    dest
}
