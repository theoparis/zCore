use crate::arch::pmgr;
use crate::arch::timer::set_next_trigger;
use crate::arch::{early_hex, early_puts};
use crate::drivers;
use crate::hal_fn::mem::phys_to_virt;
use crate::imp::config::{
    APPLE_ANS_BASE, APPLE_ANS_MBOX_BASE, APPLE_NVME_BASE, APPLE_PMGR_BASE, APPLE_PS_AIC_OFFSET,
    APPLE_PS_ANS2_OFFSET, APPLE_SART_BASE, VIRTIO_BASE,
};
use crate::KCONFIG;
use alloc::boxed::Box;
use alloc::sync::Arc;
use zcore_drivers::irq::aic::AicLayout;
use zcore_drivers::irq::gic_400;
use zcore_drivers::scheme::IrqScheme;
use zcore_drivers::uart::{AppleS5lUart, BufferedUart, Pl011Uart};
use zcore_drivers::virtio::{VirtIOHeader, VirtIoBlk};
use zcore_drivers::Device;

/// True when booted by m1n1 on Apple Silicon, where the platform has no GIC and
/// no VirtIO MMIO devices.
pub(crate) fn is_apple() -> bool {
    KCONFIG.firmware_type.contains("Apple") || KCONFIG.firmware_type.contains("Asahi")
}

pub fn init_early() {
    if is_apple() {
        let uart = AppleS5lUart::new(phys_to_virt(KCONFIG.uart_base));
        let uart = Arc::new(uart);
        drivers::add_device(Device::Uart(BufferedUart::new(uart)));
    } else {
        let uart = Pl011Uart::new(phys_to_virt(KCONFIG.uart_base));
        let uart = Arc::new(uart);
        drivers::add_device(Device::Uart(BufferedUart::new(uart)));
    }
}

/// Drain m1n1 VUART RX without an AIC UART IRQ (those land past the AIC2 mask
/// window). Safe to call from the timer / WFI path.
pub(crate) fn poll_uart() {
    if let Some(uart) = crate::drivers::all_uart().first() {
        uart.handle_irq(0);
    }
}

pub fn init() {
    if is_apple() {
        let adt = &KCONFIG.apple;
        // Everything below prefers the ADT-discovered window over the
        // compiled-in T6020 default: the addresses differ per SoC, and the
        // AIC2 register layout is not fixed at all.
        let pmgr_base = if adt.pmgr_base != 0 {
            adt.pmgr_base
        } else {
            APPLE_PMGR_BASE
        };
        let aic_base = if adt.aic_base != 0 {
            adt.aic_base
        } else {
            KCONFIG.gic_base
        };
        if aic_base != 0 {
            // AIC is power/clock-gated behind PMGR's `ps_aic` domain; touching
            // its MMIO first raises an asynchronous SError from the SoC fabric.
            pmgr::power_on(phys_to_virt(pmgr_base), APPLE_PS_AIC_OFFSET);

            // Read the capability registers here, through the unbuffered UART,
            // so a fault is attributable: reaching "regs" means reads work and
            // only the driver's writes can be at fault.
            let base = phys_to_virt(aic_base);
            early_puts("HVLOG: aic: reading caps\n");
            let ver = unsafe { core::ptr::read_volatile(base as *const u32) };
            let info1 = unsafe { core::ptr::read_volatile((base + 4) as *const u32) };
            let info3 = unsafe { core::ptr::read_volatile((base + 0xc) as *const u32) };
            early_puts("HVLOG: aic: regs ver=");
            early_hex(ver as usize);
            early_puts(" info1=");
            early_hex(info1 as usize);
            early_puts(" info3=");
            early_hex(info3 as usize);
            early_puts("\nHVLOG: aic: init\n");

            let layout = AicLayout {
                version: adt.aic_version,
                event_offset: adt.aic_event_offset,
                cap0_offset: adt.aic_cap0_offset,
                maxnumirq_offset: adt.aic_maxnumirq_offset,
                irq_cfg: adt.aic_irq_cfg,
                mask_set_stride: adt.aic_mask_set_stride,
                mask_clr_stride: adt.aic_mask_clr_stride,
            };
            let size = if adt.aic_size != 0 {
                adt.aic_size
            } else {
                0x5_0000
            };
            let aic = zcore_drivers::irq::aic::init(base, size, layout);
            early_puts("HVLOG: aic: ready\n");
            let aic = Arc::new(aic);
            drivers::add_device(Device::Irq(aic));
        }
        // Power the ANS2 domain up if it is off. The driver itself always
        // pulses the domain's PMGR reset before booting the coprocessor
        // (see the `reset_ans` closure below), so an ANS left running by a
        // previous owner — m1n1's own `nvme_init()` during chainload — gets
        // torn back down to a known state rather than adopted. `ps_afnc6_lw0`
        // is always-on; `ps_apcie_st_sys` depends on ANS2 and must not be
        // forced first.
        let pmgr_vbase = phys_to_virt(pmgr_base);
        if !pmgr::is_active(pmgr_vbase, APPLE_PS_ANS2_OFFSET) {
            early_puts("HVLOG: pmgr power ans2\n");
            pmgr::power_on(pmgr_vbase, APPLE_PS_ANS2_OFFSET);
        } else {
            early_puts("HVLOG: pmgr ans2 already active\n");
        }

        // ANS2 is an RTKit coprocessor: AppleAnsNvme::new() boots it over its
        // mailbox with a SART DMA allow-list before touching any NVMe registers.
        let ans_base = if adt.ans_base != 0 {
            adt.ans_base
        } else {
            APPLE_ANS_BASE
        };
        let ans_mbox = if adt.ans_base != 0 {
            adt.ans_base + 0x8000
        } else {
            APPLE_ANS_MBOX_BASE
        };
        let nvme_base = if adt.nvme_base != 0 {
            adt.nvme_base
        } else {
            APPLE_NVME_BASE
        };
        let sart_base = if adt.sart_base != 0 {
            adt.sart_base
        } else {
            APPLE_SART_BASE
        };
        let ans_v = phys_to_virt(ans_base);
        let mbox_v = phys_to_virt(ans_mbox);
        let nvme_v = phys_to_virt(nvme_base);
        let cpu_ctrl = unsafe { core::ptr::read_volatile((ans_v + 0x44) as *const u32) };
        early_puts("HVLOG: ans cpu_ctrl=");
        early_hex(cpu_ctrl as usize);
        // Probe the mailbox CONTROL register (32-bit) *before* any FIFO write.
        // If this SErrors, L2C_ERR_ADR will be ans_mbox+0x110 rather than SEND0.
        early_puts("\nHVLOG: mbox a2i_ctrl=");
        let a2i = unsafe { core::ptr::read_volatile((mbox_v + 0x110) as *const u32) };
        early_hex(a2i as usize);
        early_puts(" i2a_ctrl=");
        let i2a = unsafe { core::ptr::read_volatile((mbox_v + 0x114) as *const u32) };
        early_hex(i2a as usize);
        early_puts("\nHVLOG: nvme boot_status=");
        let boot = unsafe { core::ptr::read_volatile((nvme_v + 0x1300) as *const u32) };
        early_hex(boot as usize);
        early_puts("\nHVLOG: ans: init nvme\n");
        match zcore_drivers::nvme::AppleAnsNvme::new(
            phys_to_virt(nvme_base),
            nvme_base,
            phys_to_virt(ans_base),
            ans_base,
            phys_to_virt(ans_mbox),
            ans_mbox,
            phys_to_virt(sart_base),
            sart_base,
            &|| {
                early_puts("HVLOG: pmgr reset ans2\n");
                pmgr::reset(pmgr_vbase, APPLE_PS_ANS2_OFFSET);
            },
        ) {
            Ok(nvme) => {
                early_puts("HVLOG: ans: nvme ready\n");
                drivers::add_device(Device::Block(Arc::new(nvme)));
            }
            Err(e) => error!("Apple ANS NVMe init failed: {e:?}"),
        }

        #[cfg(feature = "graphic")]
        if adt.fb_base != 0 {
            use zcore_drivers::display::UefiDisplay;
            use zcore_drivers::prelude::{ColorFormat, DisplayInfo};

            early_puts("HVLOG: video: registering boot framebuffer\n");
            let display = Arc::new(UefiDisplay::new(DisplayInfo {
                width: adt.fb_width as u32,
                height: adt.fb_height as u32,
                pitch: adt.fb_stride as u32,
                // iBoot's boot framebuffer is 32bpp XRGB (byte order
                // B,G,R,X in memory); ARGB8888 already matches this for
                // the x86_64 UEFI GOP path (see that platform's
                // `drivers.rs`), so the alpha/pad byte is simply unused.
                format: ColorFormat::ARGB8888,
                fb_base_vaddr: phys_to_virt(adt.fb_base),
                fb_size: (adt.fb_stride * adt.fb_height).max(crate::PAGE_SIZE),
            }));
            drivers::add_device(Device::Display(display.clone()));
            crate::console::init_graphic_console(display);
            early_puts("HVLOG: video: ready\n");
        }
        return;
    }

    if KCONFIG.gic_base != 0 {
        let gic = gic_400::init(
            phys_to_virt(KCONFIG.gic_base + 0x1_0000),
            phys_to_virt(KCONFIG.gic_base),
        );
        gic.irq_enable(30);
        gic.irq_enable(33);
        gic.register_handler(33, Box::new(handle_uart_irq)).ok();
        gic.register_handler(30, Box::new(set_next_trigger)).ok();
        drivers::add_device(Device::Irq(Arc::new(gic)));
    }

    let virtio_blk = Arc::new(
        VirtIoBlk::new(unsafe { &mut *(phys_to_virt(VIRTIO_BASE) as *mut VirtIOHeader) }).unwrap(),
    );
    drivers::add_device(Device::Block(virtio_blk));
}

fn handle_uart_irq() {
    crate::drivers::all_uart().first_unwrap().handle_irq(0);
}
