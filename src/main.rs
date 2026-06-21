#![no_std]
#![no_main]

use embedded_hal::{delay::DelayNs, digital::OutputPin};
use panic_halt as _;
use rp235x_hal::{self as hal, Clock};

use hal::fugit::RateExtU32;
use hal::uart::{DataBits, StopBits, UartConfig, ValidatedPinRx, ValidatedPinTx};

// TEMP
use usb_device::{class_prelude::*, prelude::*};
use usbd_serial::SerialPort;

#[unsafe(link_section = ".start_block")]
#[used]
pub static IMAGE_DEF: hal::block::ImageDef = hal::block::ImageDef::secure_exe();

const XTAL_FREQ_HZ: u32 = 12_000_000;

const VOLUME: u16 = 20;

const CMD_NEXT: u8 = 0x01;
const CMD_PREV: u8 = 0x02;
const CMD_SET_VOL: u8 = 0x06;
const CMD_PLAY: u8 = 0x0d;
const CMD_STANDBY: u8 = 0x0a;

const CMD_QUERY_FILE_COUNT: u8 = 0x49;

#[hal::entry]
fn main() -> ! {
    let mut pac = hal::pac::Peripherals::take().unwrap();
    let mut watchdog = hal::Watchdog::new(pac.WATCHDOG);

    let clocks = hal::clocks::init_clocks_and_plls(
        XTAL_FREQ_HZ,
        pac.XOSC,
        pac.CLOCKS,
        pac.PLL_SYS,
        pac.PLL_USB,
        &mut pac.RESETS,
        &mut watchdog,
    )
    .unwrap();

    let mut timer = hal::Timer::new_timer0(pac.TIMER0, &mut pac.RESETS, &clocks);

    let sio = hal::Sio::new(pac.SIO);

    let pins = hal::gpio::Pins::new(
        pac.IO_BANK0,
        pac.PADS_BANK0,
        sio.gpio_bank0,
        &mut pac.RESETS,
    );

    // TEMP USB
    let usb_bus = UsbBusAllocator::new(hal::usb::UsbBus::new(
        pac.USB,
        pac.USB_DPRAM,
        clocks.usb_clock,
        true,
        &mut pac.RESETS,
    ));
    let mut serial = SerialPort::new(&usb_bus);
    let mut usb_dev = UsbDeviceBuilder::new(&usb_bus, UsbVidPid(0x16c0, 0x27dd))
        .strings(&[StringDescriptors::default()
            .manufacturer("maciek")
            .product("pico-audio")
            .serial_number("001")])
        .unwrap()
        .max_packet_size_0(64)
        .unwrap()
        .device_class(2)
        .build();

    let uart_tx = pins.gpio0.into_function();
    let uart_rx = pins.gpio1.into_function();
    let uart0 = hal::uart::UartPeripheral::new(pac.UART0, (uart_tx, uart_rx), &mut pac.RESETS)
        .enable(
            UartConfig::new(9600.Hz(), DataBits::Eight, None, StopBits::One),
            clocks.peripheral_clock.freq(),
        )
        .unwrap();

    let mut led_pin = pins.gpio25.into_push_pull_output();

    uart0.write_full_blocking(&cmd_packet(CMD_SET_VOL, VOLUME));
    timer.delay_ms(100);
    uart0.write_full_blocking(&cmd_packet(CMD_PLAY, 0));
    timer.delay_ms(100);

    let mut acc = 0;

    uart0.write_full_blocking(&cmd_packet(CMD_QUERY_FILE_COUNT, 0));
    timer.delay_ms(20);

    let mut read_buf = [0; 256];
    let mut len = None;
    if let Ok(l) = uart0.read_raw(&mut read_buf) {
        len = Some(l);
    }

    loop {
        // led_pin.set_high().unwrap();
        // timer.delay_ms(500);
        // led_pin.set_low().unwrap();
        // timer.delay_ms(500);
        // acc += 1000;
        // if acc > 5000 {
        //     acc = 0;

        //     // uart0.write_full_blocking(&cmd_packet(CMD_NEXT, 0));
        // }

        if let Some(len) = len {
            let _ = serial.write(&read_buf[..len]);
            let _ = serial.write(b"\r\n");
        } else {
            let _ = serial.write(b"AAA");
            let _ = serial.write(b"\r\n");
            // uart0.write_full_blocking(&cmd_packet(CMD_QUERY_FILE_COUNT, 0));
            // timer.delay_ms(50);

            // if let Ok(l) = uart0.read_raw(&mut read_buf) {
            //     len = Some(l);
            // }
        }

        if usb_dev.poll(&mut [&mut serial]) {
            let mut buf = [0u8; 64];
            match serial.read(&mut buf) {
                _ => (),
            }
        }
    }
}

fn cmd_packet(cmd: u8, param: u16) -> [u8; 10] {
    let len = 0x06;
    let ver = 0xff;
    let checksum = ver as u16 + len as u16 + cmd as u16 + param;
    let checksum = -(checksum as i16);
    let ch = (checksum >> 8) as u8;
    let cl = checksum as u8;
    let ph = (param >> 8) as u8;
    let pl = param as u8;
    [0x7e, ver, len, cmd, 0, ph, pl, ch, cl, 0xef]
}

#[unsafe(link_section = ".bi_entries")]
#[used]
pub static PICOTOOL_ENTRIES: [hal::binary_info::EntryAddr; 5] = [
    hal::binary_info::rp_cargo_bin_name!(),
    hal::binary_info::rp_cargo_version!(),
    hal::binary_info::rp_program_description!(c"Blinky Example"),
    hal::binary_info::rp_cargo_homepage_url!(),
    hal::binary_info::rp_program_build_attribute!(),
];
