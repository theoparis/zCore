use crate::context::TrapReason;
use crate::{Info, Kind, Source, KCONFIG};
use cortex_a::registers::{CNTP_CTL_EL0, FAR_EL1};
use tock_registers::interfaces::Readable;
use trapframe::TrapFrame;
use zcore_drivers::irq::gic_400::get_irq_num;

/// Returns the vector of the interrupt that is currently pending, or 0 if none.
///
/// Apple cores have no GIC: every interrupt arrives as an **FIQ** and is read
/// from the AIC event register. The ARM generic timer is the exception — it is
/// wired straight to the FIQ pin and is *not* an AIC event, so it produces no
/// event-register entry and must be polled from `CNTP_CTL_EL0` first. Without
/// that check a timer FIQ would read the AIC event register, get 0 ("spurious"),
/// and return with the comparator still expired, re-entering immediately.
pub fn pending_irq() -> usize {
    use crate::hal_fn::mem::phys_to_virt;

    if !super::drivers::is_apple() {
        return get_irq_num(
            phys_to_virt(KCONFIG.gic_base + 0x1_0000),
            phys_to_virt(KCONFIG.gic_base),
        );
    }

    if CNTP_CTL_EL0.is_set(CNTP_CTL_EL0::ISTATUS) && !CNTP_CTL_EL0.is_set(CNTP_CTL_EL0::IMASK) {
        return crate::timer_interrupt_vector();
    }

    let adt = &KCONFIG.apple;
    let aic_base = if adt.aic_base != 0 {
        adt.aic_base
    } else {
        KCONFIG.gic_base
    };
    if aic_base == 0 {
        return 0;
    }
    // `aic-iack-offset` from the ADT; 0xc000 is only the T6020/T8112 default
    // used when the ADT was unreadable.
    let event_offset = if adt.aic_event_offset != 0 {
        adt.aic_event_offset
    } else {
        0xc000
    };
    zcore_drivers::irq::aic::get_irq_num(phys_to_virt(aic_base + event_offset))
}

#[no_mangle]
pub extern "C" fn trap_handler(tf: &mut TrapFrame) {
    let info = Info {
        source: Source::from(tf.trap_num & 0xffff),
        kind: Kind::from((tf.trap_num >> 16) & 0xffff),
    };
    trace!("Exception from {:?}", info.source);
    match info.kind {
        Kind::Synchronous => {
            sync_handler(tf);
        }
        // Apple cores signal *all* interrupts, including the generic timer, on
        // the FIQ pin rather than IRQ, so both kinds are dispatched the same way.
        Kind::Irq | Kind::Fiq => {
            let vector = pending_irq();
            if vector != 0 {
                crate::interrupt::handle_irq(vector);
            }
        }
        _ => {
            panic!(
                "Unsupported exception type: {:?}, TrapFrame: {:?}",
                info.kind, tf
            );
        }
    }
    trace!("Exception end");
}

fn breakpoint(elr: &mut usize) {
    info!("Exception::Breakpoint: A breakpoint set @0x{:x} ", elr);
    *elr += 4;
}

fn sync_handler(tf: &mut TrapFrame) {
    match TrapReason::from(tf.trap_num) {
        TrapReason::PageFault(vaddr, flags) => crate::KHANDLER.handle_page_fault(vaddr, flags),
        TrapReason::SoftwareBreakpoint => breakpoint(&mut tf.elr),
        other => error!(
            "Unsupported trap in kernel: {:?}, FAR_EL1: {:#x?}",
            other,
            FAR_EL1.get()
        ),
    }
}
