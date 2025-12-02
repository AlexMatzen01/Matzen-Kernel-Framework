mod gdt;
mod interrupts;

use bootloader::BootInfo;

pub fn init(_boot_info: &'static BootInfo) {
    gdt::init();
    interrupts::init_idt();
    interrupts::enable();

    log::info!("arch::x86_64 initialized");
}
