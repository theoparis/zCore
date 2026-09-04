//! Kernel configuration.
use crate::PAGE_SIZE;

/// Kernel configuration passed by kernel when calls [`crate::primary_init_early()`].
#[repr(C)]
#[derive(Debug, Clone)]
pub struct KernelConfig {
    /// boot cmd line
    pub cmdline: &'static str,
    /// firmware type
    pub firmware_type: &'static str,
    /// UART base address
    pub uart_base: usize,
    /// GIC base address
    pub gic_base: usize,
    /// phystovirt offset
    pub phys_to_virt_offset: usize,
    /// Apple Silicon MMIO windows and AIC register layout, discovered from
    /// iBoot's ADT. All-zero on non-Apple platforms.
    pub apple: AppleMmio,
}

/// Apple SoC MMIO windows and the AIC2 register-block layout.
///
/// The AIC2 layout is die-specific: `irq_cfg`/strides come from the ADT
/// (`extint-baseaddress`, `extintrcfg-stride`, `intmaskset-stride`,
/// `intmaskclear-stride`), matching `aic23_init()` in m1n1's `src/aic.c`.
/// A zero field means "not present in the ADT"; the driver then falls back to
/// the layout derived from `MAXNUMIRQ`, as Linux's `irq-apple-aic.c` does.
#[derive(Debug, Clone, Default)]
pub struct AppleMmio {
    /// `/arm-io/aic` reg 0.
    pub aic_base: usize,
    /// AIC generation from `compatible`: 1, 2 or 3. Zero when unknown.
    pub aic_version: u32,
    pub aic_size: usize,
    /// `aic-iack-offset`: event register offset within the AIC window.
    pub aic_event_offset: usize,
    /// `cap0-offset`: NR_IRQ / LAST_DIE capability register offset.
    pub aic_cap0_offset: usize,
    /// `maxnumirq-offset`: MAX_IRQ / MAX_DIE capability register offset.
    pub aic_maxnumirq_offset: usize,
    /// `extint-baseaddress`: start of the per-IRQ config array.
    pub aic_irq_cfg: usize,
    pub aic_cfg_stride: usize,
    pub aic_mask_set_stride: usize,
    pub aic_mask_clr_stride: usize,
    /// `/arm-io/uart0` interrupt number.
    pub uart_irq: usize,
    /// `/arm-io/pmgr` reg 0 (power-state syscon).
    pub pmgr_base: usize,
    pub pmgr_size: usize,
    /// `/arm-io/ans` reg 0: ANS2 ASC coprocessor window (mailbox at +0x8000).
    pub ans_base: usize,
    pub ans_size: usize,
    /// `/arm-io/ans` reg 3: NVMe + NVMMU registers.
    pub nvme_base: usize,
    pub nvme_size: usize,
    /// `/arm-io/sart-ans` reg 0: ANS2 DMA allow-list.
    pub sart_base: usize,
    pub sart_size: usize,
    /// iBoot/m1n1-configured boot framebuffer (`BootArgs.video`), already
    /// painted and scanned out by the display co-processor before the
    /// kernel starts. Zero when no framebuffer was reported. 32bpp XRGB,
    /// consumed as `ColorFormat::ARGB8888` (see `kernel-hal drivers.rs`).
    pub fb_base: usize,
    pub fb_stride: usize,
    pub fb_width: usize,
    pub fb_height: usize,
    pub fb_depth: usize,
}

pub const PHYS_MEMORY_BASE: usize = 0x4000_0000;
pub const UART_SIZE: usize = 0x1000;
pub const VIRTIO_BASE: usize = 0x0a00_0000;
pub const VIRTIO_SIZE: usize = 0x100;
pub const APPLE_NVME_BASE: usize = 0x3_4bcc_0000;
pub const APPLE_NVME_SIZE: usize = 0x4_0000;
pub const APPLE_ANS_BASE: usize = 0x3_4740_0000;
pub const APPLE_ANS_SIZE: usize = 0x4000;
/// ANS2 `apple,asc-mailbox-v4` MMIO window (`ans_mbox` in t602x-nvme.dtsi).
pub const APPLE_ANS_MBOX_BASE: usize = 0x3_4740_8000;
pub const APPLE_ANS_MBOX_SIZE: usize = 0x4000;
/// ANS2 SART (DMA allow-list) MMIO window (`sart` in t602x-nvme.dtsi).
pub const APPLE_SART_BASE: usize = 0x3_4bc5_0000;
pub const APPLE_SART_SIZE: usize = 0x1_0000;
/// T6020 (M2 Pro) die-0 PMGR syscon MMIO base and size (`&pmgr` in
/// t602x-die0.dtsi). Peripherals with a `power-domains` reference (AIC,
/// ANS2, ...) are power/clock-gated behind here until explicitly enabled;
/// touching their MMIO first raises an asynchronous SError.
pub const APPLE_PMGR_BASE: usize = 0x2_8e08_0000;
pub const APPLE_PMGR_SIZE: usize = 0x8000;
/// `ps_aic` power-controller offset within `APPLE_PMGR_BASE`.
pub const APPLE_PS_AIC_OFFSET: usize = 0x108;
/// PMGR power-controller offsets for ANS2 / Storage NVMe on T6020.
pub const APPLE_PS_AFNC6_LW0_OFFSET: usize = 0x158;
pub const APPLE_PS_APCIE_ST_OFFSET: usize = 0x1a0;
pub const APPLE_PS_ANS2_OFFSET: usize = 0x1a8;
pub const APPLE_PS_APCIE_ST_SYS_OFFSET: usize = 0x408;
pub const APPLE_PS_APCIE_ST1_SYS_OFFSET: usize = 0x410;
/// A 40-bit mask silently truncates physical addresses on parts with a larger
/// PARange — Apple M2 Pro RAM lives above 0x1_0000_0000_0000 / uses 42-bit
/// PAs, and truncation turns `phys_to_virt` results into unmapped addresses.
pub const PHYS_ADDR_BITS: usize = 48;
pub const PHYS_ADDR_MAX: usize = (1 << PHYS_ADDR_BITS) - 1;
pub const PHYS_ADDR_MASK: usize = PHYS_ADDR_MAX & !(PAGE_SIZE - 1);
pub const PHYS_MEMORY_END: usize = PHYS_MEMORY_BASE + 100 * 1024 * 1024;
pub const USER_TABLE_FLAG: usize = 0xabcd_0000_0000_0000;
