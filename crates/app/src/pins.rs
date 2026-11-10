ariel_os::hal::define_peripherals!(SmartLed {
    neo: GPIO2,
    rmt: RMT
});

// #[cfg(context = "esp")]
// pub type TestUart<'a> = uart::UART0<'a>;

ariel_os::hal::define_peripherals!(Linky {
    rx: GPIO4,
    dummy_tx: GPIO0,
    lp_uart: LP_UART,
    uart: UART0,
    lp_core: LP_CORE,
    lpwr: LPWR,
});
