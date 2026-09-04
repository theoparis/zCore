//! Apple S5L UART driver (used by Apple Silicon / Asahi Linux / m1n1)
//! Compatible with S5L / Samsung S3C / Exynos style UART MMIO interface.

use crate::scheme::{impl_event_scheme, Scheme, UartScheme};
use crate::utils::EventListener;
use crate::DeviceResult;
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

// S5L UART Register Offsets
const UTRSTAT: usize = 0x10; // Tx/Rx status
const UFSTAT: usize = 0x18; // FIFO status
const UTXH: usize = 0x20; // Transmit buffer
const URXH: usize = 0x24; // Receive buffer

// UTRSTAT bits
const UTRSTAT_RXDR: u32 = 1 << 0; // Receive data ready

// UFSTAT bits (S5L)
const UFSTAT_RXCNT: u32 = 0x0f; // RX FIFO count mask (bits 3..0)
const UFSTAT_TXFULL: u32 = 1 << 9; // TX FIFO full
/// Upper bound on TX-FIFO polling iterations before writing anyway.
const TX_WAIT_SPINS: usize = 100_000;

pub struct AppleS5lUart {
    base: usize,
    listener: EventListener,
    at_line_start: AtomicBool,
}

impl AppleS5lUart {
    pub fn new(base: usize) -> Self {
        let uart = Self {
            base,
            listener: EventListener::new(),
            at_line_start: AtomicBool::new(true),
        };
        // Enable RX and TX in IRQ/polling mode:
        // - UCON_RXMODE = 1 (IRQ or Polling)
        // - UCON_TXMODE = 1 (IRQ or Polling)
        // - UCON_RXTO_ENA = (1 << 9) (RX Timeout interrupt)
        // - UCON_RXTHRESH_ENA = (1 << 12) (RX FIFO threshold interrupt)
        const UCON: usize = 0x04;
        const UCON_RXMODE_IRQ: u32 = 1;
        const UCON_TXMODE_IRQ: u32 = 1 << 2;
        const UCON_RXTO_ENA: u32 = 1 << 9;
        const UCON_RXTHRESH_ENA: u32 = 1 << 12;
        uart.write_reg(
            UCON,
            UCON_RXMODE_IRQ | UCON_TXMODE_IRQ | UCON_RXTO_ENA | UCON_RXTHRESH_ENA,
        );
        uart
    }

    #[inline]
    fn read_reg(&self, reg: usize) -> u32 {
        unsafe { ptr::read_volatile((self.base + reg) as *const u32) }
    }

    #[inline]
    fn write_reg(&self, reg: usize, val: u32) {
        unsafe { ptr::write_volatile((self.base + reg) as *mut u32, val) }
    }

    pub fn putchar(&self, c: u8) {
        // Wait for space in the TX FIFO, but never indefinitely: m1n1's VUART
        // emulation does not implement UFSTAT_TXFULL the way real S5L hardware
        // does, and spinning forever on it wedges the console.
        for _ in 0..TX_WAIT_SPINS {
            if (self.read_reg(UFSTAT) & UFSTAT_TXFULL) == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        self.write_reg(UTXH, c as u32);
    }

    pub fn getchar(&self) -> Option<u8> {
        // In m1n1 VUART emulation, UTRSTAT_RXDR (bit 0) or UFSTAT_RXCNT (bits 3..0) indicates incoming data.
        if (self.read_reg(UTRSTAT) & UTRSTAT_RXDR) != 0
            || (self.read_reg(UFSTAT) & UFSTAT_RXCNT) != 0
        {
            Some((self.read_reg(URXH) & 0xff) as u8)
        } else {
            None
        }
    }
}

impl Scheme for AppleS5lUart {
    fn name(&self) -> &str {
        "apple-s5l-uart"
    }

    fn handle_irq(&self, _irq_num: usize) {
        self.listener.trigger(())
    }
}

impl_event_scheme!(AppleS5lUart);

impl UartScheme for AppleS5lUart {
    fn try_recv(&self) -> DeviceResult<Option<u8>> {
        Ok(self.getchar())
    }

    fn send(&self, ch: u8) -> DeviceResult {
        self.putchar(ch);
        Ok(())
    }

    fn write_str(&self, s: &str) -> DeviceResult {
        // m1n1 only copies guest TX onto the hypervisor console if the line
        // starts with `HVLOG: `. Userspace and the log crate do not add that
        // themselves, so prefix here. Bytes still also go to USB VUART.
        for c in s.bytes() {
            if self.at_line_start.swap(false, Ordering::Relaxed) {
                for b in b"HVLOG: " {
                    self.putchar(*b);
                }
            }
            if c == b'\n' {
                self.putchar(b'\r');
                self.putchar(c);
                self.at_line_start.store(true, Ordering::Relaxed);
            } else {
                self.putchar(c);
            }
        }
        Ok(())
    }
}
