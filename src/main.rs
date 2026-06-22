#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use critical_section::Mutex;
use embedded_hal::delay::DelayNs;
use panic_halt as _;
use rp235x_hal::rom_data::sys_info_api::boot_random;
use rp235x_hal::{self as hal, Clock};

use hal::fugit::RateExtU32;
use hal::uart::{DataBits, StopBits, UartConfig};

// TEMP
use usb_device::{class_prelude::*, prelude::*};
use usbd_serial::SerialPort;

#[unsafe(link_section = ".start_block")]
#[used]
pub static IMAGE_DEF: hal::block::ImageDef = hal::block::ImageDef::secure_exe();

const XTAL_FREQ_HZ: u32 = 12_000_000;

const VOLUME: u16 = 10;

const CMD_PLAY_NEXT: u8 = 0x01;
const CMD_PLAY_PREV: u8 = 0x02;
const CMD_SET_VOL: u8 = 0x06;
const CMD_PLAY_TRACK: u8 = 0x03;
const CMD_PLAY: u8 = 0x0d;
const CMD_PAUSE: u8 = 0x0e;
const CMD_STANDBY: u8 = 0x0a;
const CMD_QUERY_FILE_COUNT: u8 = 0x48;

const RESP_PLAY_FINISHED: u8 = 0x3d;

static IS_PLAYNG: AtomicBool = AtomicBool::new(false);
static TRACK: AtomicU16 = AtomicU16::new(1);
static TRACK_COUNT: AtomicU16 = AtomicU16::new(1);

static READ_BUF: Mutex<[u8; 256]> = Mutex::new([0; 256]);

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

    let uart_tx = pins.gpio16.into_function();
    let uart_rx = pins.gpio17.into_function();
    let uart0 = hal::uart::UartPeripheral::new(pac.UART0, (uart_tx, uart_rx), &mut pac.RESETS)
        .enable(
            UartConfig::new(9600.Hz(), DataBits::Eight, None, StopBits::One),
            clocks.peripheral_clock.freq(),
        )
        .unwrap();

    // Wait for mp3 init.
    timer.delay_ms(1500);

    let mut read_buf = [0; 256];

    uart0.write_full_blocking(&cmd_packet(CMD_QUERY_FILE_COUNT, 0));
    timer.delay_ms(50);

    if let Ok(l) = uart0.read_raw(&mut read_buf) {
        let (_cmd, value) = parse_response(&read_buf[..l]);
        TRACK_COUNT.store(value, Ordering::Relaxed);
    }

    if let Ok(Some(r)) = boot_random() {
        let initial = (r.0 as u16) % TRACK_COUNT.load(Ordering::Relaxed);
        TRACK.store(initial, Ordering::Relaxed);
    }

    timer.delay_ms(100);
    uart0.write_full_blocking(&cmd_packet(CMD_SET_VOL, VOLUME));

    // Set interrupts.

    loop {
        if IS_PLAYNG.load(Ordering::Relaxed) {
            if let Ok(l) = uart0.read_raw(&mut read_buf) {
                let (cmd, _value) = parse_response(&read_buf[..l]);
                if cmd == RESP_PLAY_FINISHED {
                    IS_PLAYNG.store(false, Ordering::Relaxed);
                    let _ = serial.write(b"FINISHED\r\n");
                }
            }

            if usb_dev.poll(&mut [&mut serial]) {
                let mut buf = [0u8; 64];
                match serial.read(&mut buf) {
                    _ => (),
                }
            }

            // TODO add timer delay when usb debug is removed.
        } else {
            // Wait for button press to start playing.
            rp235x_hal::arch::wfi();
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

/// Returns (op_code, value)
fn parse_response(packet: &[u8]) -> (u8, u16) {
    if packet.len() < 10 {
        return 0;
    }

    let cmd = packet[3];

    let msb = packet[5] as u16;
    let lsb = packet[6] as u16;

    // TODO checksum?
    (cmd, msb << 8 | lsb)
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
