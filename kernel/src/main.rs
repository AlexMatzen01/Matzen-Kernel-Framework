#![no_std]
#![no_main]

extern crate mfk_kernel;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
