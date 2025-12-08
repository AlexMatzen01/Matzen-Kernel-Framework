//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Real-Time Clock (RTC) Driver
//!
//! Reads date and time from the CMOS RTC hardware.
//! The RTC is accessed through I/O ports 0x70 (index) and 0x71 (data).

use x86_64::instructions::port::Port;

/// CMOS address port
const CMOS_ADDRESS: u16 = 0x70;
/// CMOS data port
const CMOS_DATA: u16 = 0x71;

/// RTC register addresses
const RTC_SECONDS: u8 = 0x00;
const RTC_MINUTES: u8 = 0x02;
const RTC_HOURS: u8 = 0x04;
const RTC_DAY: u8 = 0x07;
const RTC_MONTH: u8 = 0x08;
const RTC_YEAR: u8 = 0x09;
const RTC_CENTURY: u8 = 0x32; // May not exist on all systems
const RTC_STATUS_A: u8 = 0x0A;
const RTC_STATUS_B: u8 = 0x0B;

/// Date and time structure
#[derive(Debug, Clone, Copy)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl DateTime {
    /// Returns the day of week (0 = Sunday, 6 = Saturday)
    pub fn day_of_week(&self) -> u8 {
        // Zeller's formula for Gregorian calendar
        let mut y = self.year as i32;
        let mut m = self.month as i32;

        if m < 3 {
            m += 12;
            y -= 1;
        }

        let q = self.day as i32;
        let k = y % 100;
        let j = y / 100;

        let h = (q + (13 * (m + 1)) / 5 + k + k / 4 + j / 4 - 2 * j) % 7;

        // Convert from Zeller (0=Sat) to standard (0=Sun)
        ((h + 6) % 7) as u8
    }

    /// Returns the name of the day of week
    pub fn day_name(&self) -> &'static str {
        match self.day_of_week() {
            0 => "Sunday",
            1 => "Monday",
            2 => "Tuesday",
            3 => "Wednesday",
            4 => "Thursday",
            5 => "Friday",
            6 => "Saturday",
            _ => "Unknown",
        }
    }

    /// Returns the name of the month
    pub fn month_name(&self) -> &'static str {
        match self.month {
            1 => "January",
            2 => "February",
            3 => "March",
            4 => "April",
            5 => "May",
            6 => "June",
            7 => "July",
            8 => "August",
            9 => "September",
            10 => "October",
            11 => "November",
            12 => "December",
            _ => "Unknown",
        }
    }
}

/// Reads a byte from a CMOS register
fn read_cmos(register: u8) -> u8 {
    let mut address_port: Port<u8> = Port::new(CMOS_ADDRESS);
    let mut data_port: Port<u8> = Port::new(CMOS_DATA);

    unsafe {
        // Disable NMI (bit 7) and select register
        address_port.write(register | 0x80);
        data_port.read()
    }
}

/// Checks if the RTC is currently updating
fn is_updating() -> bool {
    read_cmos(RTC_STATUS_A) & 0x80 != 0
}

/// Converts BCD to binary
fn bcd_to_binary(bcd: u8) -> u8 {
    ((bcd >> 4) * 10) + (bcd & 0x0F)
}

/// Reads the current date and time from the RTC
pub fn read_rtc() -> DateTime {
    // Wait for any update to complete
    while is_updating() {}

    // Read initial values
    let mut second = read_cmos(RTC_SECONDS);
    let mut minute = read_cmos(RTC_MINUTES);
    let mut hour = read_cmos(RTC_HOURS);
    let mut day = read_cmos(RTC_DAY);
    let mut month = read_cmos(RTC_MONTH);
    let mut year = read_cmos(RTC_YEAR);
    let century = read_cmos(RTC_CENTURY);

    // Read values again to ensure consistency (RTC may update between reads)
    loop {
        let last_second = second;
        let last_minute = minute;
        let last_hour = hour;
        let last_day = day;
        let last_month = month;
        let last_year = year;

        while is_updating() {}

        second = read_cmos(RTC_SECONDS);
        minute = read_cmos(RTC_MINUTES);
        hour = read_cmos(RTC_HOURS);
        day = read_cmos(RTC_DAY);
        month = read_cmos(RTC_MONTH);
        year = read_cmos(RTC_YEAR);

        if second == last_second
            && minute == last_minute
            && hour == last_hour
            && day == last_day
            && month == last_month
            && year == last_year
        {
            break;
        }
    }

    // Check if values are in BCD format
    let status_b = read_cmos(RTC_STATUS_B);
    let is_binary = status_b & 0x04 != 0;
    let is_24h = status_b & 0x02 != 0;

    // Convert from BCD if necessary
    if !is_binary {
        second = bcd_to_binary(second);
        minute = bcd_to_binary(minute);
        hour = bcd_to_binary(hour & 0x7F) | (hour & 0x80); // Preserve PM bit
        day = bcd_to_binary(day);
        month = bcd_to_binary(month);
        year = bcd_to_binary(year);
    }

    // Convert 12-hour to 24-hour if necessary
    if !is_24h && (hour & 0x80) != 0 {
        hour = ((hour & 0x7F) + 12) % 24;
    }

    // Calculate full year
    let full_year = if century != 0 && century != 0xFF {
        let cent = if !is_binary {
            bcd_to_binary(century)
        } else {
            century
        };
        (cent as u16) * 100 + (year as u16)
    } else {
        // Assume 2000s if century register not available
        2000 + (year as u16)
    };

    DateTime {
        year: full_year,
        month,
        day,
        hour,
        minute,
        second,
    }
}

/// Initializes the RTC driver
pub fn init() {
    // RTC is always available, nothing special to initialize
}
