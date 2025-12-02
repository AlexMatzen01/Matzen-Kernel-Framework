use bootloader::BootInfo;

pub fn run(_boot_info: &'static BootInfo) {
    log::info!("Boot stage: verifying boot info and memory map");

    super::runtime::health_check();
}
