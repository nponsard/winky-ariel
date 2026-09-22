#![no_main]
#![no_std]

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use esp_hal::{hmac::Error, rmt::Rmt, time::Rate};
use esp_hal_smartled::{SmartLedsAdapterAsync, buffer_size_async};
use reqwless::{
    client::{HttpClient, TlsConfig, TlsVerify},
    headers::ContentType,
    request::{Method, RequestBuilder},
};
use serde::{Deserialize, Serialize};
use smart_leds::{
    RGB8, SmartLedsWriteAsync, brightness, gamma,
    hsv::{Hsv, hsv2rgb},
};

use embedded_io_async::Read as _;

use ariel_os::{
    config::str_from_env_or,
    hal::{
        self,
        uart::{DataBits as HalDataBits, Parity, StopBits},
    },
    log::{Debug2Format, Hex, debug, info, warn},
    net,
    reexports::embassy_net::{
        dns::DnsSocket,
        tcp::client::{TcpClient, TcpClientState},
    },
    time::{Duration, Timer},
    uart::Baudrate,
};

mod pins;

static REPORT_CHANNEL: Channel<CriticalSectionRawMutex, Report, 10> = Channel::new();

const ENDPOINT_URL: &str = str_from_env_or!(
    "ENDPOINT_URL",
    "http://10.52.115.151:3000/report",
    "endpoint to send the GET request to",
);

// RFC8449: TLS 1.3 encrypted records are limited to 16 KiB + 256 bytes.
const MAX_ENCRYPTED_TLS_13_RECORD_SIZE: usize = 16640;
// Required by `embedded_tls::TlsConnection::new()`.
const TLS_READ_BUFFER_SIZE: usize = MAX_ENCRYPTED_TLS_13_RECORD_SIZE;
// Can be smaller than the read buffer (could be adjusted: trade-off between memory usage and not
// splitting large writes into multiple records).
const TLS_WRITE_BUFFER_SIZE: usize = 4096;

const TCP_BUFFER_SIZE: usize = 1024;
const HTTP_BUFFER_SIZE: usize = 1024;

const MAX_CONCURRENT_CONNECTIONS: usize = 2;

#[ariel_os::task(autostart, peripherals)]
async fn main(peripherals: pins::SmartLed) {
    // Configure RMT (Remote Control Transceiver) peripheral globally
    // <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/peripherals/rmt.html>
    let rmt: Rmt<'_, esp_hal::Async> = {
        let frequency: Rate = Rate::from_mhz(40);

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

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
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
    REPORT_CHANNEL.try_send(report);
}

fn parse_unsigned_integer(bytes: &[u8]) -> Option<u32> {
    let s = str::from_utf8(bytes)
        .ok()?
        // Remove trailling null bytes.
        .trim_matches(|c| c == char::from(0));

    str::parse(s).ok()
}

#[ariel_os::task(autostart)]
async fn sender() {
    let stack = net::network_stack().await.unwrap();

    let tcp_client_state =
        TcpClientState::<MAX_CONCURRENT_CONNECTIONS, TCP_BUFFER_SIZE, TCP_BUFFER_SIZE>::new();
    let tcp_client = TcpClient::new(stack, &tcp_client_state);
    let dns_client = DnsSocket::new(stack);

    let tls_seed: u64 = rand_core::RngCore::next_u64(&mut ariel_os::random::crypto_rng());

    let mut tls_rx_buffer = [0; TLS_READ_BUFFER_SIZE];
    let mut tls_tx_buffer = [0; TLS_WRITE_BUFFER_SIZE];

    // We do not authenticate the server in this example, as that would require setting up a PSK
    // with the server.
    let tls_verify = TlsVerify::None;
    let tls_config = TlsConfig::new(tls_seed, &mut tls_rx_buffer, &mut tls_tx_buffer, tls_verify);

    let mut client = HttpClient::new_with_tls(&tcp_client, &dns_client, tls_config);

    stack.wait_config_up().await;
    info!("Config up");

    let receiver = REPORT_CHANNEL.receiver();
    loop {
        let data = receiver.receive().await;

        match serde_json::to_vec(&data) {
            Ok(body) => {
                info!("sending via POST");
                let res = send_http_get_request(&mut client, ENDPOINT_URL, &body).await;
                info!("http res : {:?}", Debug2Format(&res));
            }
            Err(e) => {
                warn!("serde err : {:?}", Debug2Format(&e));
            }
        }
    }
}

async fn send_http_get_request(
    client: &mut HttpClient<'_, TcpClient<'_, MAX_CONCURRENT_CONNECTIONS>, DnsSocket<'_>>,
    url: &str,
    body: &[u8],
) -> Result<(), reqwless::Error> {
    let mut http_rx_buf = [0; HTTP_BUFFER_SIZE];

    let mut handle = client
        .request(Method::POST, url)
        .await?
        .body(body)
        .content_type(ContentType::ApplicationJson);

    let response = handle.send(&mut http_rx_buf).await?;

    info!("Response status: {}", response.status.0);

    if let Some(ref content_type) = response.content_type {
        info!("Response Content-Type: {}", content_type.as_str());
    }

    if let Ok(body) = response.body().read_to_end().await {
        if let Ok(body) = core::str::from_utf8(&body) {
            info!("Response body:\n{}", body);
        } else {
            info!("Received a response body, but it is not valid UTF-8");
        }
    } else {
        info!("No response body");
    }

    Ok(())
}
