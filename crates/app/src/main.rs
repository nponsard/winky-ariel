#![no_main]
#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use esp_hal::{rmt::Rmt, time::Rate};
use esp_hal_smartled::{SmartLedsAdapterAsync, buffer_size_async};
use reqwless::{
    client::{HttpClient, TlsConfig, TlsVerify},
    headers::ContentType,
    request::{Method, RequestBuilder},
};
use serde::{Deserialize, Serialize};
use smart_leds::{RGB8, SmartLedsWriteAsync, brightness, colors::BLUE, gamma};

use embedded_io_async::Read as _;

use ariel_os::{
    config::str_from_env_or,
    hal::{
        self,
        uart::{DataBits as HalDataBits, Parity, StopBits},
    },
    log::{Debug2Format, error, info, warn},
    net,
    reexports::embassy_net::{
        dns::DnsSocket,
        tcp::client::{TcpClient, TcpClientState},
    },
    time::{Duration, Timer},
    uart::Baudrate,
};
use tic_parser::{ParserError, ParserStatus, Report, TicMode, TicParser};

use crate::LedStatus::WaitingForData;

mod pins;

const MAX_REPORT_COUNT: usize = 30;
const WAIT_BETWEEN_UPDATES: Duration = Duration::from_secs(60);

static REPORT_CHANNEL: Channel<CriticalSectionRawMutex, Report, MAX_REPORT_COUNT> = Channel::new();
static LED_CHANNEL: Channel<CriticalSectionRawMutex, LedStatus, 5> = Channel::new();

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

#[derive(Debug, Clone, Copy)]
enum LedStatus {
    WaitingForData,
    ProcessingLinkyFrame,
    WaitingToSendReport,
    SendingReport,
}

// Demo backend doesn't support bulk inserts so we push multiple reports in one object.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Reports {
    pub reports: Vec<Report>,
}

fn set_led_status(status: LedStatus) {
    let _ = LED_CHANNEL.try_send(status);
}

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

    // let mut color = Hsv {
    //     hue: 0,
    //     sat: 255,
    //     val: 255,
    // };

    // led1: linky side
    let mut led1: RGB8 = BLUE;
    // led2: network side
    let mut led2: RGB8 = BLUE;

    let level = 5;

    let receiver = LED_CHANNEL.receiver();
    loop {
        // When sending to the LED, we do a gamma correction first (see smart_leds
        // documentation for details) and then limit the brightness.
        led.write(brightness(gamma([led1, led2, led2].into_iter()), level))
            .await
            .unwrap();

        // Limit time between updates
        Timer::after_millis(100).await;
        match receiver.receive().await {
            // Orange
            LedStatus::ProcessingLinkyFrame => led1 = RGB8::new(252, 141, 5),
            // Blue
            LedStatus::WaitingForData => led1 = RGB8::new(5, 231, 252),
            // Orange
            LedStatus::SendingReport => led2 = RGB8::new(252, 141, 5),
            // Blue
            LedStatus::WaitingToSendReport => led2 = RGB8::new(5, 231, 252),
        }
    }
}

// #[ariel_os::task(autostart)]
// async fn dummy_data() {
//     loop {
//         Timer::after_secs(1).await;

//         let _ = REPORT_CHANNEL.try_send(Report {
//             active_energy_total: Some(10),
//             apparent_power: Some(11),
//         });
//     }
// }
#[ariel_os::task(autostart, peripherals)]
async fn reader(peripherals: pins::Linky) {
    info!("Starting UART test");
    let standard_baudrate = Baudrate::_9600;
    let historic_baudrate = Baudrate::Hal(ariel_os::hal::uart::Baudrate::from(1200));

    let mut config = hal::uart::Config::default();
    // TODO: detect mode.
    config.baudrate = standard_baudrate;
    config.baudrate = historic_baudrate;

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

    let mut parser = TicParser::new(TicMode::Historic);
    set_led_status(WaitingForData);

    loop {
        let mut input = [0u8; 32];
        if let Ok(n) = uart.read(&mut input).await {
            match parser.add_data(&input[..n]) {
                Ok(ParserStatus::ParsedFrame(report)) => {
                    set_led_status(LedStatus::ProcessingLinkyFrame);
                    let _ = REPORT_CHANNEL.try_send(report);
                    set_led_status(LedStatus::WaitingForData);
                }
                Ok(_) => {}
                Err(ParserError::BufferFull) => {
                    error!("Buffer full");
                    parser = TicParser::new(TicMode::Historic)
                }
            }
        }
        // info!("Got: {}", Hex(input));
    }
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

    // Wait for network to connect successfully, then disconnect from it.
    stack.wait_config_up().await;
    info!("Config up");

    let receiver = REPORT_CHANNEL.receiver();

    loop {
        set_led_status(LedStatus::WaitingToSendReport);

        hal::wifi::esp_wifi::disconnect();

        info!("sender thread sleeping");
        Timer::after(WAIT_BETWEEN_UPDATES).await;
        set_led_status(LedStatus::SendingReport);

        if receiver.is_empty() {
            info!("No reports to send");
            continue;
        }

        info!("Preparing to send reports...");

        let mut reports = Vec::new();

        while !receiver.is_empty() {
            reports.push(receiver.receive().await);
        }

        hal::wifi::esp_wifi::connect();
        stack.wait_config_up().await;

        match serde_json::to_vec(&Reports { reports }) {
            Ok(body) => {
                info!("sending via POST");
                let res = send_http_put_request(&mut client, ENDPOINT_URL, &body).await;
                info!("http res : {:?}", Debug2Format(&res));
            }
            Err(e) => {
                warn!("serde err : {:?}", Debug2Format(&e));
            }
        }
    }
}

async fn send_http_put_request(
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
        if let Ok(body) = core::str::from_utf8(body) {
            info!("Response body:\n{}", body);
        } else {
            info!("Received a response body, but it is not valid UTF-8");
        }
    } else {
        info!("No response body");
    }

    Ok(())
}
