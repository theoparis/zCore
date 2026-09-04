//! Interrupts management.
use crate::HalResult;
use alloc::vec::Vec;
use cortex_a::asm::wfi;

hal_fn_impl! {
    impl mod crate::hal_fn::interrupt {
        fn wait_for_interrupt() {
            if super::drivers::is_apple() {
                super::drivers::poll_uart();
            }
            intr_on();
            wfi();
            intr_off();
            if super::drivers::is_apple() {
                super::drivers::poll_uart();
            }
        }

        fn handle_irq(vector: usize) {
            // On Apple the generic timer is an FIQ wired directly to the core,
            // not an AIC event, so there is no interrupt-controller handler to
            // dispatch to (the GIC path registers `set_next_trigger` on PPI 30,
            // but the AIC has no such line). Re-arm the comparator here instead;
            // leaving it expired would re-enter the FIQ handler forever.
            if vector == crate::timer_interrupt_vector() && super::drivers::is_apple() {
                super::drivers::poll_uart();
                super::timer::set_next_trigger();
                return;
            }
            // TODO: timer and other devices with GIC interrupt controller
            crate::drivers::all_irq().first_unwrap().handle_irq(vector);
        }

        fn intr_off() {
            unsafe {
                // `daifset` bit 1 = I, bit 0 = F. Apple delivers device and
                // timer interrupts on the FIQ pin, so masking I alone would
                // leave critical sections fully preemptible there.
                core::arch::asm!("msr daifset, #3");
            }
        }

        fn intr_on() {
            unsafe {
                core::arch::asm!("msr daifclr, #3");
            }
        }

        fn intr_get() -> bool {
            use cortex_a::registers::DAIF;
            use tock_registers::interfaces::Readable;
            !DAIF.is_set(DAIF::I)
        }

        fn send_ipi(cpuid: usize, reason: usize) -> HalResult {
            trace!("ipi [{}] => [{}]: {:x}", super::cpu::cpu_id(), cpuid, reason);
            panic!("send_ipi unsupported for aarch64");
        }

        fn ipi_reason() -> Vec<usize> {
            panic!("ipi_reason unsupported for aarch64");
        }
    }
}
