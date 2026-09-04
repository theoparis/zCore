use crate::imp::config::*;
use crate::PhysAddr;
use alloc::vec::Vec;
use core::ops::Range;

extern "C" {
    fn ekernel();
}

pub fn free_pmem_regions() -> Vec<Range<PhysAddr>> {
    let mut regions = Vec::new();
    let start = crate::addr::align_up(crate::mem::virt_to_phys(ekernel as *const () as usize));
    let end = if start >= PHYS_MEMORY_END {
        // If physical memory starts higher (e.g. Apple Silicon >= 0x1000_0000_0000 or 0x1000_00000)
        start + 128 * 1024 * 1024
    } else {
        PHYS_MEMORY_END
    };
    regions.push(start as PhysAddr..end as PhysAddr);
    regions
}

/// Flush the physical frame.
pub fn frame_flush(_target: PhysAddr) {
    unimplemented!()
}
