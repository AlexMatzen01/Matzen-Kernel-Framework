mod gdt;
mod interrupts;
pub mod pic;

use bootloader::BootInfo;

pub fn init(_boot_info: &'static BootInfo) {
    gdt::init();
    interrupts::init_idt();
    pic::init();
    interrupts::enable();

    log::info!("arch::x86_64 initialized");
}
