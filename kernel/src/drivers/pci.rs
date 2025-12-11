//! Basic PCI device enumeration
//!
//! Provides simple PCI configuration space access

use x86_64::instructions::port::Port;

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
    pub bar0: u32,
}

impl PciDevice {
    pub fn read_config(&self, offset: u8) -> u32 {
        let address = 0x80000000u32
            | ((self.bus as u32) << 16)
            | ((self.device as u32) << 11)
            | ((self.function as u32) << 8)
            | ((offset as u32) & 0xFC);

        unsafe {
            let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
            let mut data_port = Port::<u32>::new(CONFIG_DATA);
            
            addr_port.write(address);
            data_port.read()
        }
    }
}

pub fn enumerate_devices() -> alloc::vec::Vec<PciDevice> {
    let mut devices = alloc::vec::Vec::new();

    for bus in 0..256u16 {
        for device in 0..32u8 {
            for function in 0..8u8 {
                let address = 0x80000000u32
                    | ((bus as u32) << 16)
                    | ((device as u32) << 11)
                    | ((function as u32) << 8);

                let vendor_device = unsafe {
                    let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
                    let mut data_port = Port::<u32>::new(CONFIG_DATA);
                    
                    addr_port.write(address);
                    data_port.read()
                };

                let vendor_id = (vendor_device & 0xFFFF) as u16;
                let device_id = (vendor_device >> 16) as u16;

                // Check if device exists (vendor ID != 0xFFFF)
                if vendor_id == 0xFFFF {
                    continue;
                }

                // Read class code
                let class_info = unsafe {
                    let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
                    let mut data_port = Port::<u32>::new(CONFIG_DATA);
                    
                    addr_port.write(address | 0x08);
                    data_port.read()
                };

                let class_code = ((class_info >> 24) & 0xFF) as u8;
                let subclass = ((class_info >> 16) & 0xFF) as u8;

                // Read BAR0
                let bar0 = unsafe {
                    let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
                    let mut data_port = Port::<u32>::new(CONFIG_DATA);
                    
                    addr_port.write(address | 0x10);
                    data_port.read()
                };

                devices.push(PciDevice {
                    bus: bus as u8,
                    device,
                    function,
                    vendor_id,
                    device_id,
                    class_code,
                    subclass,
                    bar0,
                });
            }
        }
    }

    devices
}

pub fn find_device(vendor_id: u16, device_id: u16) -> Option<PciDevice> {
    // Only scan first few buses to avoid slow boot
    for bus in 0..8u16 {
        for device in 0..32u8 {
            for function in 0..8u8 {
                let address = 0x80000000u32
                    | ((bus as u32) << 16)
                    | ((device as u32) << 11)
                    | ((function as u32) << 8);

                let vendor_device = unsafe {
                    let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
                    let mut data_port = Port::<u32>::new(CONFIG_DATA);
                    
                    addr_port.write(address);
                    data_port.read()
                };

                let vid = (vendor_device & 0xFFFF) as u16;
                let did = (vendor_device >> 16) as u16;

                if vid == vendor_id && did == device_id {
                    // Read class code
                    let class_info = unsafe {
                        let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
                        let mut data_port = Port::<u32>::new(CONFIG_DATA);
                        
                        addr_port.write(address | 0x08);
                        data_port.read()
                    };

                    let class_code = ((class_info >> 24) & 0xFF) as u8;
                    let subclass = ((class_info >> 16) & 0xFF) as u8;

                    // Read BAR0
                    let bar0 = unsafe {
                        let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
                        let mut data_port = Port::<u32>::new(CONFIG_DATA);
                        
                        addr_port.write(address | 0x10);
                        data_port.read()
                    };

                    return Some(PciDevice {
                        bus: bus as u8,
                        device,
                        function,
                        vendor_id: vid,
                        device_id: did,
                        class_code,
                        subclass,
                        bar0,
                    });
                }
            }
        }
    }

    None
}
