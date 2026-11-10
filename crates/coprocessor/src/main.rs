#![no_std]
#![no_main]

use esp_lp_hal::{prelude::entry, uart::LpUart};

use panic_halt as _;

#[entry]
fn main(mut uart: LpUart) -> ! {
    loop {}
}
