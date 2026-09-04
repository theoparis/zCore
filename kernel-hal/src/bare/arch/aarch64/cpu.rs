//! CPU information.

use cortex_a::registers::*;
use tock_registers::interfaces::Readable;

hal_fn_impl! {
    impl mod crate::hal_fn::cpu {
        fn cpu_id() -> u8 {
            let id = MPIDR_EL1.get() & 0x3;
            id as u8
        }

        fn cpu_frequency() -> u16 {
            0
        }

        fn reset() -> ! {
            info!("reboot / returning to m1n1...");
            // Standard PSCI 0.2 calls supported by m1n1 / TF-A / hypervisors:
            // PSCI_SYSTEM_RESET = 0x8400_0009
            // PSCI_SYSTEM_OFF   = 0x8400_0008
            const PSCI_SYSTEM_RESET: usize = 0x8400_0009;
            const PSCI_SYSTEM_OFF: usize = 0x8400_0008;

            unsafe {
                // HVC is supported by m1n1 (EL2) for PSCI calls:
                core::arch::asm!(
                    "hvc #0",
                    in("x0") PSCI_SYSTEM_RESET,
                );
                // Fallback to SYSTEM_OFF
                core::arch::asm!(
                    "hvc #0",
                    in("x0") PSCI_SYSTEM_OFF,
                );
                // If m1n1 chainload.py is waiting on trap or debug breakpoint:
                core::arch::asm!("brk #0");
            }
            loop {
                core::hint::spin_loop();
            }
        }
    }
}
