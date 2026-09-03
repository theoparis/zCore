//! NVIDIA GPU PCI Device detection and capability probing
//!
//! Identifies NVIDIA GPU generations (Turing, Ampere, Ada, Hopper, Blackwell)
//! through PCI class and vendor IDs, probes BAR0 (MMIO) / BAR1 (VRAM aperture),
//! and checks for GSP (GPU System Processor) support.

use crate::bus::pci::{probe_bar_size, read_bar_addr, PortOpsImpl, PCI_ACCESS};
use pci::{Location, PCIDevice};

/// NVIDIA PCI Vendor ID
pub const NVIDIA_VENDOR_ID: u16 = 0x10DE;

/// NVIDIA GPU Architecture Generation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NvidiaChipGen {
    PreTuring, // Kepler, Maxwell, Pascal, Volta (No GSP or non-standard)
    Turing,    // TU10x (GSP supported)
    Ampere,    // GA10x (GSP default)
    Ada,       // AD10x (GSP mandatory)
    Hopper,    // GH10x
    Blackwell, // GB20x
    Unknown(u32),
}

impl NvidiaChipGen {
    /// Returns true if this architecture uses the GSP firmware interface
    pub const fn has_gsp(self) -> bool {
        matches!(
            self,
            Self::Turing | Self::Ampere | Self::Ada | Self::Hopper | Self::Blackwell
        )
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::PreTuring => "Pre-Turing (Legacy RM / Falcon)",
            Self::Turing => "Turing",
            Self::Ampere => "Ampere",
            Self::Ada => "Ada Lovelace",
            Self::Hopper => "Hopper",
            Self::Blackwell => "Blackwell",
            Self::Unknown(_) => "Unknown NVIDIA Architecture",
        }
    }
}

/// Probed NVIDIA PCI Device Information
#[derive(Debug, Clone)]
pub struct NvidiaPciDevice {
    pub loc: Location,
    pub vendor_id: u16,
    pub device_id: u16,
    pub bar0_addr: u64,
    pub bar0_size: u64,
    pub bar1_addr: u64,
    pub bar1_size: u64,
    pub chip_gen: NvidiaChipGen,
    pub chip_id: u32,
}

/// Inspect a PCI device to determine if it is an NVIDIA GPU
pub fn match_nvidia_pci(dev: &PCIDevice) -> bool {
    dev.id.vendor_id == NVIDIA_VENDOR_ID && (dev.id.class == 0x03 || dev.id.class == 0x04)
}

/// Probe NVIDIA GPU registers and BARs from PCI configuration
pub fn probe_nvidia_device(dev: &PCIDevice) -> Option<NvidiaPciDevice> {
    if !match_nvidia_pci(dev) {
        return None;
    }

    let ops = &PortOpsImpl;
    let am = PCI_ACCESS;

    // BAR0: MMIO register aperture (0x10)
    // BAR1: VRAM aperture (0x14)
    let bar0_addr = unsafe { read_bar_addr(ops, am, dev.loc, 0x10) };
    let bar0_size = unsafe { probe_bar_size(ops, am, dev.loc, 0x10) };
    let bar1_addr = unsafe { read_bar_addr(ops, am, dev.loc, 0x14) };
    let bar1_size = unsafe { probe_bar_size(ops, am, dev.loc, 0x14) };

    if bar0_addr == 0 {
        return None;
    }

    // Read PMC_BOOT_0 (register 0x0000_0000 in BAR0 MMIO)
    let vaddr_bar0 = crate::bus::phys_to_virt(bar0_addr as usize);
    let pmc_boot_0 = unsafe { core::ptr::read_volatile(vaddr_bar0 as *const u32) };
    let chip_id = (pmc_boot_0 >> 20) & 0xFFF;

    let chip_gen = match chip_id {
        0x160..=0x16F => NvidiaChipGen::Turing,
        0x170..=0x17F => NvidiaChipGen::Ampere,
        0x190..=0x19F => NvidiaChipGen::Ada,
        0x1B0..=0x1BF => NvidiaChipGen::Hopper,
        0x200..=0x20F => NvidiaChipGen::Blackwell,
        0x000..=0x15F => NvidiaChipGen::PreTuring,
        other => NvidiaChipGen::Unknown(other),
    };

    Some(NvidiaPciDevice {
        loc: dev.loc,
        vendor_id: dev.id.vendor_id,
        device_id: dev.id.device_id,
        bar0_addr,
        bar0_size,
        bar1_addr,
        bar1_size,
        chip_gen,
        chip_id,
    })
}
