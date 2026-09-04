// Rust language features implementations

use core::panic::PanicInfo;

#[cfg(all(feature = "board-apple", not(feature = "libos")))]
mod early {
    use core::fmt::{self, Write};
    use core::sync::atomic::{AtomicBool, Ordering};

    /// Writes panic output directly to the m1n1 VUART, bypassing the logging and
    /// driver stacks (which may be uninitialized or the source of the panic).
    ///
    /// m1n1's VUART proxy only forwards lines that begin with `HVLOG: `, so the
    /// prefix is re-emitted after every newline; a panic message spans several
    /// lines and the interesting ones come after the first.
    pub struct EarlyWriter {
        at_line_start: bool,
    }

    impl EarlyWriter {
        pub const fn new() -> Self {
            Self {
                at_line_start: true,
            }
        }
    }

    impl Write for EarlyWriter {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            for line in s.split_inclusive('\n') {
                if self.at_line_start {
                    crate::platform::early_print("HVLOG: ");
                }
                crate::platform::early_print(line);
                self.at_line_start = line.ends_with('\n');
            }
            Ok(())
        }
    }

    static IN_PANIC: AtomicBool = AtomicBool::new(false);

    /// Returns `false` if a panic is already being reported, i.e. the panic
    /// handler itself panicked. Prevents the unbounded recursion that otherwise
    /// overflows the boot stack and turns a diagnosable panic into a data abort.
    pub fn enter() -> bool {
        !IN_PANIC.swap(true, Ordering::SeqCst)
    }

    pub fn print_panic(info: &core::panic::PanicInfo) {
        let _ = writeln!(EarlyWriter::new(), "panic: {info}");
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    #[cfg(all(feature = "board-apple", not(feature = "libos")))]
    {
        if early::enter() {
            early::print_panic(info);
        } else {
            crate::platform::early_print("HVLOG: panic in panic handler\n");
        }
        kernel_hal::cpu::reset();
    }
    #[cfg(not(all(feature = "board-apple", not(feature = "libos"))))]
    {
        println!("\n\npanic cpu={}\n{}", kernel_hal::cpu::cpu_id(), info);
        error!("\n\n{info}");

        if cfg!(feature = "baremetal-test") {
            kernel_hal::cpu::reset();
        } else {
            loop {
                core::hint::spin_loop();
            }
        }
    }
}
