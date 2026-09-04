//! Apple ASC mailbox (`apple,asc-mailbox-v4`) driver.
//!
//! A simple two-FIFO doorbell mailbox that carries 96-bit (64+32) messages
//! between the AP and an RTKit coprocessor: the A2I FIFO is always AP -> IOP,
//! the I2A FIFO always IOP -> AP. Register layout taken from
//! `APPLE_ASC_MBOX_*` in `drivers/soc/apple/mailbox.c` (Asahi Linux), which
//! matches the `apple,asc-mailbox-v4` compatible string used by every
//! ANS2-attached mailbox on T6020 (M2 Pro).

use super::hvcall;
use core::ptr::read_volatile;

const A2I_CONTROL: usize = 0x110;
const A2I_SEND0: usize = 0x800;
const A2I_SEND1: usize = 0x808;

const I2A_CONTROL: usize = 0x114;
const I2A_RECV0: usize = 0x830;
const I2A_RECV1: usize = 0x838;

const CONTROL_FULL: u32 = 1 << 16;
const CONTROL_EMPTY: u32 = 1 << 17;

/// Spins allowed while waiting for FIFO space/data before giving up. There is
/// no HAL timer handle available at this layer, so timeouts are spin-counted
/// like the rest of this crate's Apple Silicon drivers (see `aic.rs`).
const SPIN_TIMEOUT: usize = 5_000_000;

/// A single mailbox message: a 64-bit payload plus an endpoint id carried in
/// the low byte of the hardware's 32-bit `msg1` slot (its upper bits carry
/// FIFO occupancy counters on read and must be zero on write).
#[derive(Debug, Clone, Copy, Default)]
pub struct MailboxMessage {
    pub msg0: u64,
    pub msg1: u32,
}

pub struct AppleMailbox {
    /// Virtual address; used for reads, which succeed over raw MMIO.
    base: usize,
    /// Physical address; used for writes, which the SoC fabric NAKs from an
    /// EL1 guest under m1n1's hypervisor and must instead be proxied through
    /// the HV's own EL2 access via [`hvcall`].
    phys_base: usize,
}

impl AppleMailbox {
    /// `base`: virtual address of the mailbox's own MMIO window (the
    /// `apple,asc-mailbox-v4` device-tree node's `reg`, e.g. `ans_mbox` in
    /// `t602x-nvme.dtsi`). This is *not* the coprocessor's CPU-control MMIO
    /// window. `phys_base` is the same window's physical address.
    pub fn new(base: usize, phys_base: usize) -> Self {
        Self { base, phys_base }
    }

    fn read_reg(&self, offset: usize) -> u32 {
        unsafe { read_volatile((self.base + offset) as *const u32) }
    }

    fn write_reg64(&self, offset: usize, val: u64) {
        unsafe { hvcall::hv_write(self.phys_base + offset, 8, val) };
    }

    fn read_reg64(&self, offset: usize) -> u64 {
        unsafe { read_volatile((self.base + offset) as *const u64) }
    }

    /// Sends a message, spinning until the A2I FIFO has room.
    pub fn send(&self, msg: MailboxMessage) -> bool {
        let mut spins = 0;
        while self.read_reg(A2I_CONTROL) & CONTROL_FULL != 0 {
            spins += 1;
            if spins > SPIN_TIMEOUT {
                return false;
            }
            core::hint::spin_loop();
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.write_reg64(A2I_SEND0, msg.msg0);
        self.write_reg64(A2I_SEND1, msg.msg1 as u64);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        true
    }

    /// Non-blocking receive; `None` if the I2A FIFO is empty.
    pub fn try_recv(&self) -> Option<MailboxMessage> {
        if self.read_reg(I2A_CONTROL) & CONTROL_EMPTY != 0 {
            return None;
        }
        let msg0 = self.read_reg64(I2A_RECV0);
        let msg1 = self.read_reg64(I2A_RECV1) as u32;
        Some(MailboxMessage { msg0, msg1 })
    }

    /// Receives a message, spinning up to `spins` iterations for one to
    /// arrive.
    pub fn recv_timeout(&self, spins: usize) -> Option<MailboxMessage> {
        for _ in 0..spins {
            if let Some(m) = self.try_recv() {
                return Some(m);
            }
            core::hint::spin_loop();
        }
        None
    }
}
