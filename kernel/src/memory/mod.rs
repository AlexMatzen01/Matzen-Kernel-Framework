mod layout;

use bootloader::BootInfo;

pub fn init(boot_info: &'static BootInfo) {
    log::info!(
        "Kernel memory layout: text={:#x?} data={:#x?} bss={:#x?}",
        layout::text_range(),
        layout::data_range(),
        layout::bss_range()
    );

    let regions = &boot_info.memory_map;
    log::info!("Bootloader provided {} memory regions", regions.len());
}
