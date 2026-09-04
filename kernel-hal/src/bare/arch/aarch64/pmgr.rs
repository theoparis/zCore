//! Apple SoC PMGR (Power Manager) device power-state control.
//!
//! Peripherals behind a `power-domains = <&ps_xxx>` device-tree reference are
//! power/clock-gated by default. Touching their MMIO before powering them on
//! raises an asynchronous SError from the SoC fabric (see AICv2 on T602x).
//! This mirrors `apple_pmgr_ps_set()` in
//! `drivers/pmdomain/apple/pmgr-pwrstate.c`.

use core::ptr::{read_volatile, write_volatile};

const PMGR_RESET: u32 = 1 << 31;
const PMGR_AUTO_ENABLE: u32 = 1 << 28;
const PMGR_PS_RESET: u32 = 1 << 12;
const PMGR_DEV_DISABLE: u32 = 1 << 10;
const PMGR_WAS_CLKGATED: u32 = 1 << 9;
const PMGR_WAS_PWRGATED: u32 = 1 << 8;
const PMGR_PS_ACTUAL_SHIFT: u32 = 4;
const PMGR_PS_ACTUAL_MASK: u32 = 0xf << PMGR_PS_ACTUAL_SHIFT;
const PMGR_PS_TARGET_MASK: u32 = 0xf;

const PMGR_PS_ACTIVE: u32 = 0xf;

/// Upper bound on power-state transition polling iterations, matching
/// `APPLE_PMGR_PS_SET_TIMEOUT` (100us) but spin-based since no timer is
/// available this early.
const PS_WAIT_SPINS: usize = 2_000_000;

/// Powers on the device behind a PMGR `power-controller@offset` node and
/// waits for `PS_ACTUAL` to reach the active state. `pmgr_base` is the
/// virtual address of the enclosing `pmgr` syscon MMIO region (e.g. the
/// per-die PMGR at `0x2_8e08_0000` on T6020); `offset` is the node's `reg`
/// offset within it (e.g. `0x108` for `ps_aic`).
pub fn power_on(pmgr_base: usize, offset: usize) {
    use crate::arch::{early_hex, early_puts};

    let addr = (pmgr_base + offset) as *mut u32;
    let mut reg = unsafe { read_volatile(addr) };
    early_puts("HVLOG: pmgr: ");
    early_hex(offset);
    early_puts(" pre=");
    early_hex(reg as usize);

    reg &= !(PMGR_DEV_DISABLE
        | PMGR_PS_RESET
        | PMGR_AUTO_ENABLE
        | PMGR_WAS_CLKGATED
        | PMGR_WAS_PWRGATED
        | PMGR_PS_TARGET_MASK);
    reg |= PMGR_PS_ACTIVE & PMGR_PS_TARGET_MASK;
    unsafe { write_volatile(addr, reg) };

    let mut spins = 0;
    loop {
        let cur = unsafe { read_volatile(addr) };
        if (cur & PMGR_PS_ACTUAL_MASK) >> PMGR_PS_ACTUAL_SHIFT == PMGR_PS_ACTIVE {
            early_puts(" post=");
            early_hex(cur as usize);
            early_puts(" active\n");
            return;
        }
        spins += 1;
        if spins > PS_WAIT_SPINS {
            early_puts(" post=");
            early_hex(cur as usize);
            early_puts(" TIMED OUT\n");
            return;
        }
        core::hint::spin_loop();
    }
}

/// Resets a PMGR device: pulses PMGR_RESET & PMGR_DEV_DISABLE, exactly as
/// `pmgr_reset_device()` does in m1n1 `src/pmgr.c`.
/// Pulsing just `DEV_RESET`/`DEV_DISABLE` while the power domain's
/// `PS_TARGET` stays at `ACTIVE` does *not* clear a coprocessor's internal
/// firmware state (SRAM/queue tables survive): a previous owner's live
/// ANS2 (e.g. m1n1's own `nvme_init()` during chainload) keeps its IOCQ/IOSQ
/// 1 around, and re-`CREATE_CQ`/`CREATE_SQ`ing those IDs later panics the
/// coprocessor instead of erroring, corrupting the UART with an RTKit crash
/// dump. Actually power the domain off (`PS_TARGET` = 0, wait for
/// `PS_ACTUAL` to drop out of active) before powering it back on — a true
/// power cycle — so the coprocessor reboots from a clean state.
pub fn reset(pmgr_base: usize, offset: usize) {
    use crate::arch::{early_hex, early_puts};

    let addr = (pmgr_base + offset) as *mut u32;

    // Drive PS_TARGET to 0 (off) and wait for PS_ACTUAL to leave the
    // active state.
    let reg = unsafe { read_volatile(addr) };
    let off = (reg & !(PMGR_PS_TARGET_MASK | PMGR_AUTO_ENABLE)) | PMGR_DEV_DISABLE | PMGR_PS_RESET;
    unsafe { write_volatile(addr, off) };

    let mut spins = 0;
    loop {
        let cur = unsafe { read_volatile(addr) };
        if (cur & PMGR_PS_ACTUAL_MASK) >> PMGR_PS_ACTUAL_SHIFT != PMGR_PS_ACTIVE {
            break;
        }
        spins += 1;
        if spins > PS_WAIT_SPINS {
            early_puts("HVLOG: pmgr: reset ");
            early_hex(offset);
            early_puts(" timed out waiting for power-off\n");
            break;
        }
        core::hint::spin_loop();
    }

    // Hold off briefly so the domain fully discharges before re-enabling.
    for _ in 0..10_000 {
        core::hint::spin_loop();
    }

    // Power back on, same sequence as `power_on()`.
    power_on(pmgr_base, offset);
}

/// True if the device behind the given PMGR power-controller node is
/// already active (`PS_ACTUAL == 0xf`) or has auto-enable set.
pub fn is_active(pmgr_base: usize, offset: usize) -> bool {
    let addr = (pmgr_base + offset) as *const u32;
    let reg = unsafe { read_volatile(addr) };
    ((reg & PMGR_PS_ACTUAL_MASK) >> PMGR_PS_ACTUAL_SHIFT == PMGR_PS_ACTIVE)
        || (reg & PMGR_RESET == 0 && reg & PMGR_AUTO_ENABLE != 0)
}
