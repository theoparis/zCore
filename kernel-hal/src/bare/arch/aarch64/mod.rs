pub mod config;
pub mod cpu;
pub mod drivers;
pub mod interrupt;
pub mod mem;
pub mod timer;
pub mod trap;
pub mod vm;

use crate::KCONFIG;
use crate::{mem::phys_to_virt, utils::init_once::InitOnce, PhysAddr};
use alloc::string::{String, ToString};
use core::ops::Range;

hal_fn_impl_default!(crate::hal_fn::console);

static INITRD_REGION: InitOnce<Option<Range<PhysAddr>>> = InitOnce::new_with_default(None);
static CMDLINE: InitOnce<String> = InitOnce::new_with_default(String::new());

pub fn cmdline() -> String {
    CMDLINE.clone()
}

pub fn init_ram_disk() -> Option<&'static mut [u8]> {
    INITRD_REGION.as_ref().map(|range| unsafe {
        core::slice::from_raw_parts_mut(phys_to_virt(range.start) as *mut u8, range.len())
    })
}

pub fn primary_init_early() {
    CMDLINE.init_once_by(KCONFIG.cmdline.to_string());
    drivers::init_early();
}

/// Writes directly to the UART FIFO, bypassing every driver and lock.
///
/// The only console available while the kernel page table is being switched:
/// the UART driver's buffered path needs locks and an executor, neither of
/// which is usable mid-handover. Lines carry the `HVLOG: ` prefix that m1n1's
/// VUART proxy requires to forward them.
pub fn early_puts(s: &str) {
    const UTXH: usize = 0x20;
    let base = phys_to_virt(KCONFIG.uart_base) + UTXH;
    for b in s.bytes() {
        if b == b'\n' {
            unsafe { core::ptr::write_volatile(base as *mut u32, b'\r' as u32) };
        }
        unsafe { core::ptr::write_volatile(base as *mut u32, b as u32) };
    }
}

/// Prints `v` as `0x…` through the same unbuffered path. `info!`/`error!` go
/// through a lock and an executor, so they never reach m1n1's VUART when a
/// fault follows immediately.
pub fn early_hex(v: usize) {
    const UTXH: usize = 0x20;
    let base = phys_to_virt(KCONFIG.uart_base) + UTXH;
    let put = |b: u8| unsafe { core::ptr::write_volatile(base as *mut u32, b as u32) };
    put(b'0');
    put(b'x');
    if v == 0 {
        put(b'0');
        return;
    }
    let mut started = false;
    for shift in (0..16).rev() {
        let nibble = ((v >> (shift * 4)) & 0xf) as u8;
        started |= nibble != 0;
        if started {
            put(if nibble < 10 {
                b'0' + nibble
            } else {
                b'a' + nibble - 10
            });
        }
    }
}

pub fn primary_init() {
    early_puts("HVLOG: hal: vm::init\n");
    vm::init();
    early_puts("HVLOG: hal: drivers::init\n");
    drivers::init();
    early_puts("HVLOG: hal: primary_init done\n");
}

pub fn secondary_init() {
    unimplemented!()
}

pub const fn timer_interrupt_vector() -> usize {
    30
}

pub fn timer_init() {
    timer::init();
}
