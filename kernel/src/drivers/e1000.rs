//! Intel E1000 Network Driver
//! 
//! Basic driver for Intel E1000 network card (commonly used in QEMU)

use x86_64::instructions::port::{Port, PortReadOnly, PortWriteOnly};
use x86_64::PhysAddr;
use spin::Mutex;
use lazy_static::lazy_static;
use alloc::vec::Vec;

const E1000_VENDOR_ID: u16 = 0x8086;
const E1000_DEVICE_ID: u16 = 0x100E;

// E1000 Registers
const REG_CTRL: u32 = 0x0000;
const REG_STATUS: u32 = 0x0008;
const REG_EEPROM: u32 = 0x0014;
const REG_CTRL_EXT: u32 = 0x0018;
const REG_IMASK: u32 = 0x00D0;
const REG_RCTRL: u32 = 0x0100;
const REG_RXDESCLO: u32 = 0x2800;
const REG_RXDESCHI: u32 = 0x2804;
const REG_RXDESCLEN: u32 = 0x2808;
const REG_RXDESCHEAD: u32 = 0x2810;
const REG_RXDESCTAIL: u32 = 0x2818;
const REG_TCTRL: u32 = 0x0400;
const REG_TXDESCLO: u32 = 0x3800;
const REG_TXDESCHI: u32 = 0x3804;
const REG_TXDESCLEN: u32 = 0x3808;
const REG_TXDESCHEAD: u32 = 0x3810;
const REG_TXDESCTAIL: u32 = 0x3818;
const REG_RDTR: u32 = 0x2820;
const REG_RXDCTL: u32 = 0x3828;
const REG_RADV: u32 = 0x282C;
const REG_RSRPD: u32 = 0x2C00;

// Control bits
const CTRL_SLU: u32 = 0x40;
const RCTL_EN: u32 = 1 << 1;
const RCTL_SBP: u32 = 1 << 2;
const RCTL_UPE: u32 = 1 << 3;
const RCTL_MPE: u32 = 1 << 4;
const RCTL_LPE: u32 = 1 << 5;
const RCTL_BAM: u32 = 1 << 15;
const RCTL_BSIZE_2048: u32 = 0 << 16;
const RCTL_BSIZE_1024: u32 = 1 << 16;
const RCTL_BSIZE_512: u32 = 2 << 16;
const RCTL_BSIZE_256: u32 = 3 << 16;
const RCTL_SECRC: u32 = 1 << 26;

const TCTL_EN: u32 = 1 << 1;
const TCTL_PSP: u32 = 1 << 3;

// Descriptor counts
const RX_DESC_COUNT: usize = 32;
const TX_DESC_COUNT: usize = 8;

// Buffer size
const BUFFER_SIZE: usize = 2048;

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct RxDescriptor {
    addr: u64,
    length: u16,
    checksum: u16,
    status: u8,
    errors: u8,
    special: u16,
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct TxDescriptor {
    addr: u64,
    length: u16,
    cso: u8,
    cmd: u8,
    status: u8,
    css: u8,
    special: u16,
}

pub struct E1000 {
    mem_base: usize,
    mac_address: [u8; 6],
    rx_descriptors: Vec<RxDescriptor>,
    tx_descriptors: Vec<TxDescriptor>,
    rx_buffers: Vec<Vec<u8>>,
    tx_buffers: Vec<Vec<u8>>,
    rx_current: usize,
    tx_current: usize,
}

impl E1000 {
    pub fn new(mem_base: usize) -> Self {
        let mut driver = E1000 {
            mem_base,
            mac_address: [0; 6],
            rx_descriptors: alloc::vec![RxDescriptor {
                addr: 0,
                length: 0,
                checksum: 0,
                status: 0,
                errors: 0,
                special: 0,
            }; RX_DESC_COUNT],
            tx_descriptors: alloc::vec![TxDescriptor {
                addr: 0,
                length: 0,
                cso: 0,
                cmd: 0,
                status: 0,
                css: 0,
                special: 0,
            }; TX_DESC_COUNT],
            rx_buffers: alloc::vec![alloc::vec![0u8; BUFFER_SIZE]; RX_DESC_COUNT],
            tx_buffers: alloc::vec![alloc::vec![0u8; BUFFER_SIZE]; TX_DESC_COUNT],
            rx_current: 0,
            tx_current: 0,
        };

        driver.init();
        driver
    }

    fn read_reg(&self, reg: u32) -> u32 {
        unsafe {
            core::ptr::read_volatile((self.mem_base + reg as usize) as *const u32)
        }
    }

    fn write_reg(&self, reg: u32, value: u32) {
        unsafe {
            core::ptr::write_volatile((self.mem_base + reg as usize) as *mut u32, value);
        }
    }

    fn read_eeprom(&self, addr: u8) -> u16 {
        self.write_reg(REG_EEPROM, 1 | ((addr as u32) << 8));
        
        // Wait for read to complete
        let mut tmp = 0;
        while (tmp & (1 << 4)) == 0 {
            tmp = self.read_reg(REG_EEPROM);
        }
        
        ((tmp >> 16) & 0xFFFF) as u16
    }

    fn read_mac_address(&mut self) {
        let mac_low = self.read_eeprom(0);
        let mac_mid = self.read_eeprom(1);
        let mac_high = self.read_eeprom(2);

        self.mac_address[0] = (mac_low & 0xFF) as u8;
        self.mac_address[1] = (mac_low >> 8) as u8;
        self.mac_address[2] = (mac_mid & 0xFF) as u8;
        self.mac_address[3] = (mac_mid >> 8) as u8;
        self.mac_address[4] = (mac_high & 0xFF) as u8;
        self.mac_address[5] = (mac_high >> 8) as u8;
    }

    fn init(&mut self) {
        // Read MAC address
        self.read_mac_address();

        // Enable bus mastering and memory access
        self.write_reg(REG_CTRL, self.read_reg(REG_CTRL) | CTRL_SLU);

        // Setup receive descriptors
        for i in 0..RX_DESC_COUNT {
            let phys_addr = &self.rx_buffers[i][0] as *const u8 as u64;
            self.rx_descriptors[i].addr = phys_addr;
            self.rx_descriptors[i].status = 0;
        }

        let rx_desc_addr = self.rx_descriptors.as_ptr() as u64;
        self.write_reg(REG_RXDESCLO, (rx_desc_addr & 0xFFFFFFFF) as u32);
        self.write_reg(REG_RXDESCHI, (rx_desc_addr >> 32) as u32);
        self.write_reg(REG_RXDESCLEN, (RX_DESC_COUNT * 16) as u32);
        self.write_reg(REG_RXDESCHEAD, 0);
        self.write_reg(REG_RXDESCTAIL, (RX_DESC_COUNT - 1) as u32);

        // Setup transmit descriptors
        for i in 0..TX_DESC_COUNT {
            let phys_addr = &self.tx_buffers[i][0] as *const u8 as u64;
            self.tx_descriptors[i].addr = phys_addr;
            self.tx_descriptors[i].status = 1; // DD bit
            self.tx_descriptors[i].cmd = 0;
        }

        let tx_desc_addr = self.tx_descriptors.as_ptr() as u64;
        self.write_reg(REG_TXDESCLO, (tx_desc_addr & 0xFFFFFFFF) as u32);
        self.write_reg(REG_TXDESCHI, (tx_desc_addr >> 32) as u32);
        self.write_reg(REG_TXDESCLEN, (TX_DESC_COUNT * 16) as u32);
        self.write_reg(REG_TXDESCHEAD, 0);
        self.write_reg(REG_TXDESCTAIL, 0);

        // Enable receive
        self.write_reg(REG_RCTRL, RCTL_EN | RCTL_SBP | RCTL_UPE | RCTL_MPE | 
                                  RCTL_BAM | RCTL_BSIZE_2048 | RCTL_SECRC);

        // Enable transmit
        self.write_reg(REG_TCTRL, TCTL_EN | TCTL_PSP | (15 << 4) | (64 << 12));
    }

    pub fn mac_address(&self) -> [u8; 6] {
        self.mac_address
    }

    pub fn send_packet(&mut self, data: &[u8]) -> Result<(), &'static str> {
        if data.len() > BUFFER_SIZE {
            return Err("Packet too large");
        }

        let desc_index = self.tx_current;
        
        // Wait for descriptor to be available
        if self.tx_descriptors[desc_index].status & 1 == 0 {
            return Err("TX queue full");
        }

        // Copy data to buffer
        self.tx_buffers[desc_index][..data.len()].copy_from_slice(data);

        // Setup descriptor
        self.tx_descriptors[desc_index].length = data.len() as u16;
        self.tx_descriptors[desc_index].cmd = (1 << 0) | (1 << 1) | (1 << 3); // EOP, IFCS, RS
        self.tx_descriptors[desc_index].status = 0;

        // Update tail
        self.tx_current = (self.tx_current + 1) % TX_DESC_COUNT;
        self.write_reg(REG_TXDESCTAIL, self.tx_current as u32);

        Ok(())
    }

    pub fn receive_packet(&mut self) -> Option<Vec<u8>> {
        let desc_index = self.rx_current;

        // Check if descriptor has data
        if self.rx_descriptors[desc_index].status & 1 == 0 {
            return None;
        }

        let length = self.rx_descriptors[desc_index].length as usize;
        let packet = self.rx_buffers[desc_index][..length].to_vec();

        // Reset descriptor
        self.rx_descriptors[desc_index].status = 0;

        // Update tail
        self.rx_current = (self.rx_current + 1) % RX_DESC_COUNT;
        self.write_reg(REG_RXDESCTAIL, ((self.rx_current + RX_DESC_COUNT - 1) % RX_DESC_COUNT) as u32);

        Some(packet)
    }
}

lazy_static! {
    pub static ref E1000_DRIVER: Mutex<Option<E1000>> = Mutex::new(None);
}

pub fn init(phys_mem_offset: u64) -> Result<(), &'static str> {
    crate::serial_println!("E1000: Starting initialization...");
    
    // Try to find E1000 device via PCI
    crate::serial_println!("E1000: Scanning PCI bus...");
    let pci_device = crate::drivers::pci::find_device(E1000_VENDOR_ID, E1000_DEVICE_ID);
    
    if pci_device.is_none() {
        crate::serial_println!("E1000: Device not found on PCI bus");
        return Err("E1000 device not found");
    }
    
    crate::serial_println!("E1000: Device found!");
    let pci_dev = pci_device.unwrap();
    let mem_base = (pci_dev.bar0 & !0xF) as u64; // Mask off lower bits (they're flags)
    
    if mem_base == 0 {
        crate::serial_println!("E1000: Invalid BAR0 address");
        return Err("Invalid E1000 memory address");
    }
    
    crate::serial_println!("E1000 found at PCI bus {}, device {}, function {}", 
        pci_dev.bus, pci_dev.device, pci_dev.function);
    crate::serial_println!("  Physical memory base: {:#x}", mem_base);
    
    // Map physical address to virtual using the physical memory offset
    let virt_base = (phys_mem_offset + mem_base) as usize;
    crate::serial_println!("  Virtual memory base: {:#x}", virt_base);
    
    let driver = E1000::new(virt_base);
    let mac = driver.mac_address();
    
    crate::serial_println!("E1000 initialized");
    crate::serial_println!("  MAC Address: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
    
    *E1000_DRIVER.lock() = Some(driver);
    Ok(())
}

pub fn send_packet(data: &[u8]) -> Result<(), &'static str> {
    let mut driver = E1000_DRIVER.lock();
    if let Some(ref mut d) = *driver {
        d.send_packet(data)
    } else {
        Err("E1000 not initialized")
    }
}

pub fn receive_packet() -> Option<Vec<u8>> {
    let mut driver = E1000_DRIVER.lock();
    if let Some(ref mut d) = *driver {
        d.receive_packet()
    } else {
        None
    }
}

pub fn mac_address() -> Option<[u8; 6]> {
    let driver = E1000_DRIVER.lock();
    driver.as_ref().map(|d| d.mac_address())
}
