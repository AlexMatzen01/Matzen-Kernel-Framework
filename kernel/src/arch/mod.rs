pub mod x86_64;

use bootloader::BootInfo;

pub fn init(boot_info: &'static BootInfo) {
    x86_64::init(boot_info);
}
