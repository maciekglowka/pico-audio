#![no_std]
#![no_main]

use core::cell::RefCell;
use core::sync::atomic::{AtomicU16, Ordering};
use critical_section::Mutex;
use embedded_hal::delay::DelayNs;
use panic_halt as _;
use rp235x_hal::rom_data::sys_info_api::boot_random;
use rp235x_hal::{self as hal, gpio, Clock};

use hal::fugit::RateExtU32;
use hal::uart::{DataBits, StopBits, UartConfig};

#[cfg(feature = "led")]
use embedded_hal::digital::StatefulOutputPin;

#[cfg(feature = "usb")]
use usb_device::{class_prelude::*, prelude::*};
#[cfg(feature = "usb")]
use usbd_serial::SerialPort;

#[unsafe(link_section = ".start_block")]
#[used]
pub static IMAGE_DEF: hal::block::ImageDef = hal::block::ImageDef::secure_exe();

const XTAL_FREQ_HZ: u32 = 12_000_000;

const VOLUME: u16 = 20;

const CMD_SET_VOL: u8 = 0x06;
const CMD_PLAY_TRACK: u8 = 0x03;
const CMD_PAUSE: u8 = 0x0e;
const CMD_STANDBY: u8 = 0x0a;
const CMD_WAKEUP: u8 = 0x0b;
const CMD_QUERY_FILE_COUNT: u8 = 0x48;

const RESP_PLAY_FINISHED: u8 = 0x3d;

const STATE_PAUSE: u16 = 0;
const STATE_PLAYING: u16 = 1;
const STATE_ABOUT_TO_PLAY: u16 = 2;
const STATE_ABOUT_TO_PAUSE: u16 = 3;

static PLAY_STATE: AtomicU16 = AtomicU16::new(STATE_PAUSE);
static TRACK: AtomicU16 = AtomicU16::new(1);
static TRACK_COUNT: AtomicU16 = AtomicU16::new(1);

type ButtonPrevPin = gpio::Pin<gpio::bank0::Gpio5, gpio::FunctionSioInput, gpio::PullUp>;
type ButtonPlayPin = gpio::Pin<gpio::bank0::Gpio9, gpio::FunctionSioInput, gpio::PullUp>;
type ButtonNextPin = gpio::Pin<gpio::bank0::Gpio13, gpio::FunctionSioInput, gpio::PullUp>;

#[cfg(feature = "led")]
type LedPin = gpio::Pin<gpio::bank0::Gpio25, gpio::FunctionSioOutput, gpio::PullNone>;

struct ButtonPins {
    prev_pin: ButtonPrevPin,
    play_pin: ButtonPlayPin,
    next_pin: ButtonNextPin,
    #[cfg(feature = "led")]
    led_pin: LedPin,
}

static BUTTON_STATE: Mutex<RefCell<Option<ButtonPins>>> = Mutex::new(RefCell::new(None));

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

    #[cfg(feature = "usb")]
    let usb_bus = UsbBusAllocator::new(hal::usb::UsbBus::new(
        pac.USB,
        pac.USB_DPRAM,
        clocks.usb_clock,
        true,
        &mut pac.RESETS,
    ));
    #[cfg(feature = "usb")]
    let mut serial = SerialPort::new(&usb_bus);
    #[cfg(feature = "usb")]
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

    // Set uart.
    let uart_tx = pins.gpio16.into_function();
    let uart_rx = pins.gpio17.into_function();
    let uart0 = hal::uart::UartPeripheral::new(pac.UART0, (uart_tx, uart_rx), &mut pac.RESETS)
        .enable(
            UartConfig::new(9600.Hz(), DataBits::Eight, None, StopBits::One),
            clocks.peripheral_clock.freq(),
        )
        .unwrap();

    // Set buttons.

    let prev_pin = pins.gpio5.reconfigure();
    prev_pin.set_interrupt_enabled(gpio::Interrupt::EdgeHigh, true);

    let play_pin = pins.gpio9.reconfigure();
    play_pin.set_interrupt_enabled(gpio::Interrupt::EdgeHigh, true);

    let next_pin = pins.gpio13.reconfigure();
    next_pin.set_interrupt_enabled(gpio::Interrupt::EdgeHigh, true);

    #[cfg(feature = "led")]
    let led_pin = pins.gpio25.reconfigure();

    critical_section::with(|cs| {
        BUTTON_STATE.borrow(cs).replace(Some(ButtonPins {
            prev_pin,
            play_pin,
            next_pin,
            #[cfg(feature = "led")]
            led_pin,
        }));
    });

    // Wait for mp3 init.
    timer.delay_ms(1500);

    let mut read_buf = [0; 256];
    let _ = uart0.read_raw(&mut read_buf);

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
    unsafe {
        hal::arch::interrupt_unmask(hal::pac::Interrupt::IO_IRQ_BANK0);
    }
    unsafe {
        hal::arch::interrupt_enable();
    }

    loop {
        match PLAY_STATE.load(Ordering::Relaxed) {
            STATE_PAUSE => {
                // Wait for button press to start playing.
                #[cfg(not(feature = "usb"))]
                hal::arch::wfi();
            }
            STATE_ABOUT_TO_PAUSE => {
                #[cfg(feature = "usb")]
                let _ = serial.write(b"PAUSE\r\n");

                PLAY_STATE.store(STATE_PAUSE, Ordering::Relaxed);
                uart0.write_full_blocking(&cmd_packet(CMD_PAUSE, 0));

                // #[cfg(not(feature = "usb"))]
                // timer.delay_ms(100);
                // #[cfg(not(feature = "usb"))]
                // uart0.write_full_blocking(&cmd_packet(CMD_STANDBY, 0));
            }
            STATE_ABOUT_TO_PLAY => {
                PLAY_STATE.store(STATE_PLAYING, Ordering::Relaxed);

                // #[cfg(not(feature = "usb"))]
                // uart0.write_full_blocking(&cmd_packet(CMD_WAKEUP, 0));
                // #[cfg(not(feature = "usb"))]
                // timer.delay_ms(100);

                uart0.write_full_blocking(&cmd_packet(
                    CMD_PLAY_TRACK,
                    TRACK.load(Ordering::Relaxed),
                ));

                #[cfg(feature = "usb")]
                let _ = serial.write(b"PLAY\r\n");
            }
            STATE_PLAYING => {
                if let Ok(l) = uart0.read_raw(&mut read_buf) {
                    let (cmd, _value) = parse_response(&read_buf[..l]);
                    if cmd == RESP_PLAY_FINISHED {
                        PLAY_STATE.store(STATE_ABOUT_TO_PAUSE, Ordering::Relaxed);

                        // Advance track for next play.
                        let track = TRACK.load(Ordering::Relaxed);
                        let track_count = TRACK_COUNT.load(Ordering::Relaxed);
                        if track >= track_count {
                            // 1-indexed
                            TRACK.store(1, Ordering::Relaxed);
                        } else {
                            TRACK.store(track + 1, Ordering::Relaxed);
                        }

                        #[cfg(feature = "usb")]
                        let _ = serial.write(b"FINISHED\r\n");
                    }
                }
            }
            _ => (),
        }

        #[cfg(not(feature = "usb"))]
        timer.delay_ms(10);

        #[cfg(feature = "usb")]
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

/// Returns (op_code, value)
fn parse_response(packet: &[u8]) -> (u8, u16) {
    if packet.len() < 10 {
        return (0, 0);
    }

    let cmd = packet[3];

    let msb = packet[5] as u16;
    let lsb = packet[6] as u16;

    // TODO checksum?
    (cmd, msb << 8 | lsb)
}

fn change_track(track: u16) {
    TRACK.store(track, Ordering::Relaxed);
    PLAY_STATE.store(STATE_ABOUT_TO_PLAY, Ordering::Relaxed);
}

/// Interrupt handler.
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
fn IO_IRQ_BANK0() {
    critical_section::with(|cs| {
        if let Some(state) = BUTTON_STATE.borrow_ref_mut(cs).as_mut() {
            if state.prev_pin.interrupt_status(gpio::Interrupt::EdgeHigh) {
                // TODO atomic operations needed?
                let track = TRACK.load(Ordering::Relaxed);
                let track_count = TRACK_COUNT.load(Ordering::Relaxed);
                if track <= 1 {
                    // 1-indexed
                    change_track(track_count);
                } else {
                    change_track(track - 1);
                }
                #[cfg(feature = "led")]
                state.led_pin.toggle();

                state.prev_pin.clear_interrupt(gpio::Interrupt::EdgeHigh);
            }
            if state.play_pin.interrupt_status(gpio::Interrupt::EdgeHigh) {
                match PLAY_STATE.load(Ordering::Relaxed) {
                    STATE_PAUSE | STATE_ABOUT_TO_PAUSE => {
                        PLAY_STATE.store(STATE_ABOUT_TO_PLAY, Ordering::Relaxed)
                    }
                    STATE_PLAYING | STATE_ABOUT_TO_PLAY => {
                        PLAY_STATE.store(STATE_ABOUT_TO_PAUSE, Ordering::Relaxed)
                    }
                    _ => (),
                }
                #[cfg(feature = "led")]
                state.led_pin.toggle();

                state.play_pin.clear_interrupt(gpio::Interrupt::EdgeHigh);
            }
            if state.next_pin.interrupt_status(gpio::Interrupt::EdgeHigh) {
                let track = TRACK.load(Ordering::Relaxed);
                let track_count = TRACK_COUNT.load(Ordering::Relaxed);
                if track >= track_count {
                    // 1-indexed
                    change_track(1);
                } else {
                    change_track(track + 1);
                }
                #[cfg(feature = "led")]
                state.led_pin.toggle();

                state.next_pin.clear_interrupt(gpio::Interrupt::EdgeHigh);
            }
        }
    })
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
