#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![feature(abi_x86_interrupt)]
#![test_runner(crate::tests::run)]
#![reexport_test_harness_main = "test_main"]

mod arch;
mod core;
mod drivers;
mod intrinsics;
mod logger;
mod memory;
mod panic;
#[cfg(test)]
mod tests;

use bootloader::{entry_point, BootInfo};
use ::core::sync::atomic::{AtomicBool, Ordering};

static KERNEL_READY: AtomicBool = AtomicBool::new(false);

entry_point!(kernel_entry);

fn kernel_entry(boot_info: &'static BootInfo) -> ! {
    logger::init_once();
    log::info!("Matzen Kernel Framework booting...");

    arch::init(boot_info);
    memory::init(boot_info);
    core::boot::run(boot_info);

    #[cfg(test)]
    test_main();

    KERNEL_READY.store(true, Ordering::Release);
    log::info!("Kernel is now idling; ready for next subsystems.");
    core::runtime::idle_loop();
}

pub fn ready() -> bool {
    KERNEL_READY.load(Ordering::Acquire)
}
