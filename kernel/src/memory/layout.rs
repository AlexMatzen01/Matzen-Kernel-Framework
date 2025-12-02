use core::ops::Range;

extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __data_start: u8;
    static __data_end: u8;
    static __bss_start: u8;
    static __bss_end: u8;
}

fn range(start: &u8, end: &u8) -> Range<usize> {
    (start as *const _ as usize)..(end as *const _ as usize)
}

pub fn text_range() -> Range<usize> {
    unsafe { range(&__text_start, &__text_end) }
}

pub fn data_range() -> Range<usize> {
    unsafe { range(&__data_start, &__data_end) }
}

pub fn bss_range() -> Range<usize> {
    unsafe { range(&__bss_start, &__bss_end) }
}
