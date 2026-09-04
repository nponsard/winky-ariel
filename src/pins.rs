use ariel_os::hal::{uart, peripherals};

ariel_os::hal::define_peripherals!(SmartLed {
    neo: GPIO2,
    rmt: RMT
});

#[cfg(context = "esp")]
pub type TestUart<'a> = uart::UART0<'a>;

ariel_os::hal::define_peripherals!(Linky {
    rx: GPIO4,
    dummy_tx: GPIO10
});
