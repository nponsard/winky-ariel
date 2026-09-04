#![no_main]
#![no_std]

use core::str::Chars;

use esp_hal::{rmt::Rmt, time::Rate};
use esp_hal_smartled::{SmartLedsAdapterAsync, buffer_size_async};
use smart_leds::{
    RGB8, SmartLedsWriteAsync, brightness, gamma,
    hsv::{Hsv, hsv2rgb},
};

use embedded_io_async::Read as _;

use ariel_os::{
    hal::{
        self,
        uart::{DataBits as HalDataBits, Parity, StopBits},
    },
    log::{Debug2Format, Hex, debug, info, warn},
    time::{Duration, Timer},
    uart::Baudrate,
};

mod pins;

#[ariel_os::task(autostart, peripherals)]
async fn main(peripherals: pins::SmartLed) {
    // Configure RMT (Remote Control Transceiver) peripheral globally
    // <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/peripherals/rmt.html>
    let rmt: Rmt<'_, esp_hal::Async> = {
        let frequency: Rate = Rate::from_mhz(80);

        Rmt::new(peripherals.rmt, frequency)
    }
    .expect("Failed to initialize RMT")
    .into_async();

    // We use one of the RMT channels to instantiate a `SmartLedsAdapterAsync` which canbuffer_size_async
    // be used directly with all `smart_led` implementations
    let rmt_channel = rmt.channel0;
    let mut rmt_buffer = [esp_hal::rmt::PulseCode::default(); buffer_size_async(3)];

    let mut led = SmartLedsAdapterAsync::new(rmt_channel, peripherals.neo, &mut rmt_buffer);

    let mut color = Hsv {
        hue: 0,
        sat: 255,
        val: 255,
    };
    let mut data: RGB8;
    let level = 10;

    loop {
        for hue in 0..=255 {
            color.hue = hue;

            // info!("hue: {}", hue);
            // Convert from the HSV color space (where we can easily transition from one
            // color to the other) to the RGB color space that we can then send to the LED
            data = hsv2rgb(color);

            // info!("data: {:?}", Debug2Format(&data));

            // When sending to the LED, we do a gamma correction first (see smart_leds
            // documentation for details) and then limit the brightness to 10 out of 255 so
            // that the output is not too bright.
            led.write(brightness(gamma([data, data, data].into_iter()), level))
                .await
                .unwrap();
            Timer::after(Duration::from_millis(10)).await;
        }
    }
}

// New linky separators

const FRAME_START: u8 = 0x02;
const FRAME_END: u8 = 0x03;
// LF
const DATA_START: u8 = 0x0A;
// CR
const DATA_END: u8 = 0x0D;
// HT
const SEPARATOR: u8 = 0x09;

const LABEL_MAX_SIZE: usize = 16;
const DATA_MAX_SIZE: usize = 100;

const PROCESS_BUFFER_SIZE: usize = 4096;

// // Wh
// const ACTIVE_ENERGY_TOTAL_LABEL: &str = "EAST";
// // VA
// const APPARENT_POWER_LABEL: &str = "SINSTS";

#[derive(Debug, Clone, Copy, Default)]
pub struct Report {
    /// Label: EAST, unit: Wh
    pub active_energy_total: Option<u32>,
    /// Label: SINSTS, unit: VA
    pub apparent_power: Option<u32>,
}

#[ariel_os::task(autostart, peripherals)]
async fn reader(peripherals: pins::Linky) {
    info!("Starting UART test");

    let mut config = hal::uart::Config::default();
    config.baudrate = Baudrate::_9600;
    config.data_bits = HalDataBits::Data7;
    config.parity = Parity::Even;
    config.stop_bits = StopBits::Stop1;
    info!("Selected configuration: {:?}", config);

    let mut rx_buf = [0u8; 32];
    let mut tx_buf = [0u8; 32];

    let mut uart = pins::TestUart::new(
        peripherals.rx,
        peripherals.dummy_tx,
        &mut rx_buf,
        &mut tx_buf,
        config,
    )
    .expect("Invalid UART configuration");

    let mut current_data_index = 0;
    let mut current_data = [0u8; PROCESS_BUFFER_SIZE];

    let mut reading_frame = false;

    loop {
        let mut input = [0u8; 32];
        let n = uart.read(&mut input).await.unwrap_or(0);

        for char in input.iter() {
            if *char == FRAME_START {
                reading_frame = true;
                current_data_index = 0;
                current_data = [0; PROCESS_BUFFER_SIZE];
            } else if *char == FRAME_END {
                reading_frame = false;
                process_frame(&current_data[..current_data_index]).await;
            } else if reading_frame {
                if current_data_index >= PROCESS_BUFFER_SIZE - 1 {
                    warn!("Buffer overrun");
                    current_data_index = 0;
                    current_data = [0; PROCESS_BUFFER_SIZE];
                    reading_frame = false;
                    continue;
                }

                current_data[current_data_index] = *char;
                current_data_index += 1;
            }
        }
        // info!("Got: {}", Hex(input));
    }
}

pub struct Field {
    pub label: [u8; LABEL_MAX_SIZE],
    pub data: [u8; DATA_MAX_SIZE],
}
impl Field {
    pub fn new(ref_label: &[u8], ref_data: &[u8]) -> Self {
        let mut label = [0u8; LABEL_MAX_SIZE];
        let mut data = [0u8; DATA_MAX_SIZE];

        let len = ref_label.len().min(LABEL_MAX_SIZE);
        label[..len].copy_from_slice(ref_label);

        let len = ref_data.len().min(DATA_MAX_SIZE);
        data[..len].copy_from_slice(ref_data);

        Self { label, data }
    }
}

fn parse_buffer(buffer: &[u8]) -> impl Iterator<Item = Field> {
    buffer.split(|c| *c == DATA_START).filter_map(|d| {
        let mut split = d.split(|c| *c == SEPARATOR);

        let ref_label = split.next()?;

        let ref_time_or_data = split.next()?;
        let ref_data_or_checksum = split.next()?;

        // If we have one more field it means whe had a timestamp in there.
        let ref_data = if split.next().is_some() {
            ref_data_or_checksum
        } else {
            ref_time_or_data
        };

        Some(Field::new(ref_label, ref_data))
    })
}

async fn process_frame(buffer: &[u8]) {
    let data = unsafe { str::from_utf8_unchecked(buffer) };

    debug!("Frame: {}", data);

    let fields = parse_buffer(buffer);

    let mut report = Report::default();
    for field in fields {
        let Ok(label) = str::from_utf8(&field.label) else {
            continue;
        };
        let label = label.trim_matches(|c| c == char::from(0));
        match label {
            "SINSTS" => {
                let d = parse_unsigned_integer(&field.data);

                report.apparent_power = d;
            }
            "EAST" => {
                report.active_energy_total = parse_unsigned_integer(&field.data);
            }
            _ => {
                debug!("Ignoring {}, {}", label, Hex(label.as_bytes()));
            }
        }
    }

    info!("report : {:?}", Debug2Format(&report));
}

fn parse_unsigned_integer(bytes: &[u8]) -> Option<u32> {
    let s = str::from_utf8(bytes)
        .ok()?
        // Remove trailling null bytes.
        .trim_matches(|c| c == char::from(0));

    str::parse(s).ok()
}
