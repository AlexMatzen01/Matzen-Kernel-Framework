use core::panic::PanicInfo;

use crate::core::runtime;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    log::error!("Kernel panic: {}", info);
    runtime::halt()
}
