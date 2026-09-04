use crate::arch::timer::set_next_trigger;
use crate::drivers;
use crate::hal_fn::mem::phys_to_virt;
use crate::imp::config::VIRTIO_BASE;
use crate::KCONFIG;
use alloc::boxed::Box;
use alloc::sync::Arc;
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

pub(crate) fn poll_uart() {
    if let Some(uart) = crate::drivers::all_uart().first() {
        uart.handle_irq(0);
    }
}

pub fn init() {
    if is_apple() {
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
