use core::ops::Range;

// Note: When using the bootloader crate, we don't have direct access to linker symbols
// since the bootloader manages the linking. We provide placeholder implementations here.
// The actual kernel memory information is available via BootInfo.

/// Returns a placeholder range for the text section.
/// Use BootInfo from the bootloader crate for actual memory mapping.
pub fn text_range() -> Range<usize> {
    0..0
}

/// Returns a placeholder range for the data section.
/// Use BootInfo from the bootloader crate for actual memory mapping.
pub fn data_range() -> Range<usize> {
    0..0
}

/// Returns a placeholder range for the bss section.
/// Use BootInfo from the bootloader crate for actual memory mapping.
pub fn bss_range() -> Range<usize> {
    0..0
}
