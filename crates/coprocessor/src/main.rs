#![no_std]
#![no_main]

use embedded_io::Read;
use esp_lp_hal::{prelude::entry, uart::LpUart};

use panic_halt as _;
use tic_parser::{ParserStatus, TicMode, TicParser};

#[entry]
fn main(mut uart: LpUart) -> ! {
    let mut parser = TicParser::new(TicMode::Historic);
    loop {
        let mut input = [0u8; 32];

        if let Ok(n) = uart.read(&mut input) {
            if let Ok(ParserStatus::ParsedFrame(report)) = parser.add_data(&input[..n]) {

            }
        }
    }
}
