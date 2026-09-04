//! Apple Interrupt Controller (AIC / AIC2) Driver
//!
//! AIC is used across Apple Silicon processors (M1/M2/M3 family):
//! - AIC1: T8103 (M1), T8015, T8010
//! - AIC2: T8112 (M2), T6000/T6001/T6002 (M1 Pro/Max/Ultra), T6020/T6021/T6022 (M2 Pro/Max/Ultra)

use core::ptr::{read_volatile, write_volatile};

use crate::prelude::IrqHandler;
use crate::scheme::{IrqScheme, Scheme};
use crate::sync::Mutex;
use crate::utils::IrqManager;
use crate::DeviceResult;

// AIC v1 registers
pub const AIC_INFO: usize = 0x0004;
pub const AIC_CONFIG: usize = 0x0010;
pub const AIC_WHOAMI: usize = 0x2000;
pub const AIC_EVENT: usize = 0x2004;
pub const AIC_IPI_SEND: usize = 0x2008;
pub const AIC_IPI_ACK: usize = 0x200c;
pub const AIC_IPI_MASK_SET: usize = 0x2024;
pub const AIC_IPI_MASK_CLR: usize = 0x2028;
pub const AIC_TARGET_CPU: usize = 0x3000;

// AIC v2 registers
pub const AIC2_VERSION: usize = 0x0000;
pub const AIC2_INFO1: usize = 0x0004;
pub const AIC2_INFO2: usize = 0x0008;
pub const AIC2_INFO3: usize = 0x000c;
pub const AIC2_RESET: usize = 0x0010;
pub const AIC2_CONFIG: usize = 0x0014;
pub const AIC2_CONFIG_ENABLE: u32 = 1 << 0;
pub const AIC2_CONFIG_PREFER_PCPU: u32 = 1 << 28;
pub const AIC2_IRQ_CFG: usize = 0x2000;

pub const AIC_MAX_IRQ: usize = 1024;

// Event type constants
pub const AIC_EVENT_TYPE_FIQ: u32 = 0;
pub const AIC_EVENT_TYPE_IRQ: u32 = 1;
pub const AIC_EVENT_TYPE_IPI: u32 = 4;

/// AIC register-block layout as described by iBoot's ADT.
///
/// AIC2's layout is die-specific, so m1n1's `aic23_init()` reads it from the
/// ADT rather than assuming it. A zero field means "absent"; the driver then
/// derives that value from the capability registers.
#[derive(Debug, Clone, Copy, Default)]
pub struct AicLayout {
    /// Generation from the `compatible` string (`aic,1`/`aic,2`/`aic,3`).
    ///
    /// Zero means unknown. This MUST come from the device tree: the version
    /// register's low byte is 8 on T6020, so probing it misdetects AIC2 as
    /// AIC1 and then writes AIC1's TARGET_CPU and mask arrays, which do not
    /// exist on AIC2 — the fabric answers with an asynchronous SError.
    pub version: u32,
    /// `aic-iack-offset`
    pub event_offset: usize,
    /// `cap0-offset`
    pub cap0_offset: usize,
    /// `maxnumirq-offset`
    pub maxnumirq_offset: usize,
    /// `extint-baseaddress`
    pub irq_cfg: usize,
    /// `intmaskset-stride`
    pub mask_set_stride: usize,
    /// `intmaskclear-stride`
    pub mask_clr_stride: usize,
}

pub struct AppleAic {
    base: usize,
    /// Length of the main register window; writes past it are refused rather
    /// than left to raise an asynchronous SError from the SoC fabric.
    size: usize,
    event_base: usize,
    version: u32,
    nr_irq: u32,
    max_irq: u32,
    nr_die: u32,
    #[allow(unused)]
    max_die: u32,
    mask_set_stride: usize,
    mask_clr_stride: usize,
    #[allow(unused)]
    sw_set: usize,
    sw_clr: usize,
    mask_set: usize,
    mask_clr: usize,
    #[allow(unused)]
    hw_state: usize,
    #[allow(unused)]
    target_cpu: Option<usize>,
    manager: Mutex<IrqManager<AIC_MAX_IRQ>>,
}

impl AppleAic {
    /// Initialize AIC / AIC2 from its MMIO window and the layout iBoot's ADT
    /// describes.
    ///
    /// `base`: main register window (virtual address).
    /// `size`: window length; every register write is bounds-checked against it.
    /// `layout`: ADT-provided offsets/strides. Any zero field is derived from
    /// the capability registers the way `irq-apple-aic.c` does.
    pub fn new(base: usize, size: usize, layout: AicLayout) -> Self {
        let cap0_off = if layout.cap0_offset != 0 {
            layout.cap0_offset
        } else {
            AIC2_INFO1
        };
        let info1 = unsafe { read_volatile((base + cap0_off) as *const u32) };

        // Generation comes from the device tree. The version register is NOT a
        // discriminator: T6020 reports 8 in its low byte, and `LAST_DIE` is 0
        // on a single-die part, so probing both misdetects AIC2 as AIC1.
        // m1n1's `aic_init()` likewise dispatches purely on `compatible`.
        let dt_version = layout.version;
        if dt_version == 0 {
            warn!("AIC: no generation in the device tree, assuming AIC2");
        }
        let (ver, nr_irq, max_irq, nr_die, max_die, start_off, target_cpu) = if dt_version != 1 {
            // AIC2 / AIC3
            let maxnumirq_off = if layout.maxnumirq_offset != 0 {
                layout.maxnumirq_offset
            } else {
                AIC2_INFO3
            };
            let info3 = unsafe { read_volatile((base + maxnumirq_off) as *const u32) };
            let nr_irq = info1 & 0xffff;
            let max_irq = info3 & 0xffff;
            let nr_die = ((info1 >> 24) & 0xf) + 1;
            let max_die = (info3 >> 24) & 0xf;
            let start_off = if layout.irq_cfg != 0 {
                layout.irq_cfg
            } else {
                AIC2_IRQ_CFG
            };
            let ver = if dt_version == 3 { 3 } else { 2 };
            (ver, nr_irq, max_irq, nr_die, max_die, start_off, None)
        } else {
            // AIC1
            let nr_irq = info1 & 0xffff;
            (1, nr_irq, 1024, 1, 1, AIC_TARGET_CPU, Some(AIC_TARGET_CPU))
        };

        let mut off = start_off + 4 * (max_irq as usize);
        let sw_set = off;
        off += 4 * ((max_irq as usize) >> 5);
        let sw_clr = off;
        off += 4 * ((max_irq as usize) >> 5);
        let mask_set = off;
        off += 4 * ((max_irq as usize) >> 5);
        let mask_clr = off;
        off += 4 * ((max_irq as usize) >> 5);
        let hw_state = off;
        off += 4 * ((max_irq as usize) >> 5);
        let derived_stride = off - start_off;

        // The event register sits at `aic-iack-offset` on AIC2/3 and at a fixed
        // offset on AIC1. There is no safe default for AIC2: a wrong guess reads
        // an undecoded address.
        let event_offset = if layout.event_offset != 0 {
            layout.event_offset
        } else if ver == 1 {
            AIC_EVENT
        } else {
            0xc000
        };

        let mut aic = Self {
            base,
            size,
            event_base: base + event_offset,
            version: ver,
            nr_irq,
            max_irq,
            nr_die,
            max_die,
            mask_set_stride: if layout.mask_set_stride != 0 {
                layout.mask_set_stride
            } else {
                derived_stride
            },
            mask_clr_stride: if layout.mask_clr_stride != 0 {
                layout.mask_clr_stride
            } else {
                derived_stride
            },
            sw_set,
            sw_clr,
            mask_set,
            mask_clr,
            hw_state,
            target_cpu,
            manager: Mutex::new(IrqManager::new(0..AIC_MAX_IRQ)),
        };

        info!(
            "AIC v{}: nr_irq={} max_irq={} nr_die={}/{} window={:#x}+{:#x} cfg={:#x} \
             sw_set={:#x} sw_clr={:#x} mask_set={:#x} mask_clr={:#x} hw_state={:#x} \
             strides={:#x}/{:#x} event=+{:#x}",
            aic.version,
            aic.nr_irq,
            aic.max_irq,
            aic.nr_die,
            aic.max_die,
            base,
            size,
            start_off,
            aic.sw_set,
            aic.sw_clr,
            aic.mask_set,
            aic.mask_clr,
            aic.hw_state,
            aic.mask_set_stride,
            aic.mask_clr_stride,
            event_offset,
        );
        aic.init();
        aic
    }

    /// Writes a register, refusing offsets outside the window the ADT
    /// describes. An out-of-window store lands in an undecoded hole and the
    /// fabric reports it as an asynchronous SError far from the faulting
    /// instruction, so catch it here instead.
    fn write_reg(&self, offset: usize, val: u32) {
        if offset + 4 > self.size {
            error!(
                "AIC: refusing write at {:#x} past {:#x}-byte window",
                offset, self.size
            );
            return;
        }
        unsafe {
            write_volatile((self.base + offset) as *mut u32, val);
        }
    }

    #[allow(unused)]
    fn read_reg(&self, offset: usize) -> u32 {
        unsafe { read_volatile((self.base + offset) as *const u32) }
    }
    fn init(&mut self) {
        info!(
            "AIC init: v{} nr_irq={} nr_die={} mask_set={:#x} sw_clr={:#x}",
            self.version, self.nr_irq, self.nr_die, self.mask_set, self.sw_clr
        );
    }

    /// Read next pending hardware event from AIC event register.
    /// Returns (die, type, irq_num). Reading acknowledges and auto-masks the IRQ.
    pub fn read_event(&self) -> (u32, u32, usize) {
        // `event_base` already includes `aic-iack-offset` (or AIC1's fixed
        // AIC_EVENT), so read it directly.
        let event = unsafe { read_volatile(self.event_base as *const u32) };
        let die = (event >> 24) & 0xff;
        let event_type = (event >> 16) & 0xff;
        let irq_num = (event & 0xffff) as usize;
        (die, event_type, irq_num)
    }

    /// Read pending hardware IRQ number, if any.
    pub fn pending_irq(&self) -> Option<usize> {
        let (_die, event_type, irq_num) = self.read_event();
        if event_type == AIC_EVENT_TYPE_IRQ {
            Some(irq_num)
        } else {
            None
        }
    }

    /// Unmask / enable the specified IRQ.
    pub fn irq_enable(&self, irq_num: usize) {
        let die = irq_num / (self.max_irq.max(1) as usize);
        let irq = irq_num % (self.max_irq.max(1) as usize);
        let bit = 1u32 << (irq & 31);
        self.write_reg(
            self.mask_clr + die * self.mask_clr_stride + (irq >> 5) * 4,
            bit,
        );
    }

    /// Mask / disable the specified IRQ.
    pub fn irq_disable(&self, irq_num: usize) {
        let die = irq_num / (self.max_irq.max(1) as usize);
        let irq = irq_num % (self.max_irq.max(1) as usize);
        let bit = 1u32 << (irq & 31);
        self.write_reg(
            self.mask_set + die * self.mask_set_stride + (irq >> 5) * 4,
            bit,
        );
    }
}

impl Scheme for AppleAic {
    fn name(&self) -> &str {
        if self.version >= 2 {
            "apple-aic2"
        } else {
            "apple-aic"
        }
    }

    fn handle_irq(&self, irq_num: usize) {
        self.manager.lock().handle(irq_num).ok();
        // EOI: reading the event auto-masked the IRQ in hardware, so unmask to re-enable
        self.irq_enable(irq_num);
    }
}

impl IrqScheme for AppleAic {
    fn is_valid_irq(&self, irq_num: usize) -> bool {
        irq_num < (self.max_irq as usize)
    }

    fn mask(&self, irq_num: usize) -> DeviceResult {
        self.irq_disable(irq_num);
        Ok(())
    }

    fn unmask(&self, irq_num: usize) -> DeviceResult {
        self.irq_enable(irq_num);
        Ok(())
    }

    fn register_handler(&self, irq_num: usize, handler: IrqHandler) -> DeviceResult {
        self.manager
            .lock()
            .register_handler(irq_num, handler)
            .map(|_| ())
    }

    fn unregister(&self, irq_num: usize) -> DeviceResult {
        self.manager.lock().unregister_handler(irq_num)
    }
}

/// Helper function to initialize Apple AIC.
pub fn init(base: usize, size: usize, layout: AicLayout) -> AppleAic {
    AppleAic::new(base, size, layout)
}

/// Helper function for trap handler to read pending IRQ.
pub fn get_irq_num(event_base: usize) -> usize {
    let event = unsafe { read_volatile(event_base as *const u32) };
    let event_type = (event >> 16) & 0xff;
    let irq_num = (event & 0xffff) as usize;
    if event_type == AIC_EVENT_TYPE_IRQ {
        irq_num
    } else {
        0
    }
}
