//! Apple SART (Secure Address Range Table) v3 DMA allow-list.
//!
//! SART is not an IOMMU: it can't remap addresses, only permit or deny DMA
//! to a physical range. RTKit coprocessors that have no DART of their own
//! (ANS2 on T602x is one) can only target memory the AP has explicitly
//! whitelisted here first.
//!
//! Register layout is `sart_ops_v3` from `drivers/soc/apple/sart.c` (Asahi
//! Linux), which is what every `apple,t6000-sart`-compatible SART uses
//! (including `apple,t6020-sart` on the M2 Pro — Linux's SART driver falls
//! back to the `t6000` compatible string for it). Older SART versions (v0 on
//! T8015, v2 on T8103) are out of scope: this driver targets T602x only.

use core::ptr::read_volatile;

const MAX_ENTRIES: usize = 16;
const CONFIG_BASE: usize = 0x00;
const PADDR_BASE: usize = 0x40;
const SIZE_BASE: usize = 0x80;
const ENTRY_STRIDE: usize = 4;

/// paddr/size are both expressed in 4K units.
const SHIFT: u32 = 12;
/// `GENMASK(29, 0)`: 30-bit size field.
const SIZE_MAX: u32 = (1 << 30) - 1;
/// The exact bit meaning is undocumented; this is the value the bootloader
/// and every known OS driver use to mean "fully allow".
const FLAGS_ALLOW: u32 = 0xff;

pub struct AppleSart {
    base: usize,
    /// Physical address of `base`, used for register writes (see
    /// [`crate::soc::hvcall`] — reads still use `base` directly).
    phys_base: usize,
    /// Entries populated by firmware before we ever touched the device;
    /// never reused or cleared.
    protected: u16,
    /// Entries we have handed out.
    used: u16,
}

impl AppleSart {
    /// `base`/`phys_base`: virtual/physical address of the SART's own MMIO
    /// window.
    pub fn new(base: usize, phys_base: usize) -> Self {
        let read_flags =
            |i: usize| -> u32 { Self::reg_read(base, CONFIG_BASE + ENTRY_STRIDE * i) & 0xff };
        let mut protected = 0u16;
        for i in 0..MAX_ENTRIES {
            if read_flags(i) != 0 {
                protected |= 1 << i;
            }
        }
        Self {
            base,
            phys_base,
            protected,
            used: 0,
        }
    }

    fn reg_read(base: usize, offset: usize) -> u32 {
        unsafe { read_volatile((base + offset) as *const u32) }
    }

    fn reg_write(&self, offset: usize, val: u32) {
        unsafe { crate::soc::hvcall::hv_write(self.phys_base + offset, 4, val as u64) };
    }

    /// Whitelists `[paddr, paddr + size)` for coprocessor DMA. Both must be
    /// 4K-aligned. Returns `false` if the range is misaligned, too large, or
    /// every entry is already in use.
    pub fn add_allowed_region(&mut self, paddr: usize, size: usize) -> bool {
        if paddr & ((1 << SHIFT) - 1) != 0 || size & ((1 << SHIFT) - 1) != 0 {
            return false;
        }
        let shifted_size = (size >> SHIFT) as u32;
        if shifted_size > SIZE_MAX {
            return false;
        }
        for i in 0..MAX_ENTRIES {
            let bit = 1 << i;
            if self.protected & bit != 0 || self.used & bit != 0 {
                continue;
            }
            self.used |= bit;
            self.reg_write(PADDR_BASE + ENTRY_STRIDE * i, (paddr >> SHIFT) as u32);
            self.reg_write(SIZE_BASE + ENTRY_STRIDE * i, shifted_size);
            self.reg_write(CONFIG_BASE + ENTRY_STRIDE * i, FLAGS_ALLOW);
            return true;
        }
        false
    }

    /// Removes a previously-added region. Returns `false` if no matching
    /// entry is found.
    pub fn remove_allowed_region(&mut self, paddr: usize, size: usize) -> bool {
        let shifted_paddr = (paddr >> SHIFT) as u32;
        let shifted_size = (size >> SHIFT) as u32;
        for i in 0..MAX_ENTRIES {
            let bit = 1 << i;
            if self.protected & bit != 0 || self.used & bit == 0 {
                continue;
            }
            let cur_paddr = Self::reg_read(self.base, PADDR_BASE + ENTRY_STRIDE * i);
            let cur_size = Self::reg_read(self.base, SIZE_BASE + ENTRY_STRIDE * i);
            if cur_paddr != shifted_paddr || cur_size != shifted_size {
                continue;
            }
            self.reg_write(CONFIG_BASE + ENTRY_STRIDE * i, 0);
            self.reg_write(PADDR_BASE + ENTRY_STRIDE * i, 0);
            self.reg_write(SIZE_BASE + ENTRY_STRIDE * i, 0);
            self.used &= !bit;
            return true;
        }
        false
    }
}
