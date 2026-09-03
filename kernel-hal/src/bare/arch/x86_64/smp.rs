//! Application processors startup for x86_64.

use core::arch::global_asm;
use x86::apic::{xapic::XAPIC, ApicControl, ApicId};

global_asm!(include_str!("boot_ap.S"));

/// Startup application processors specified in `ap_ids`.
pub unsafe fn start_application_processors(
    ap_ids: &[u8],
    entry: fn(),
    stack_fn: impl Fn(usize) -> usize,
    phys_to_virt: impl Fn(usize) -> usize,
) {
    if ap_ids.is_empty() {
        return;
    }

    unsafe {
        (phys_to_virt(0x6ff8) as *mut u32).write(x86::controlregs::cr3() as u32);
        (phys_to_virt(0x6ff0) as *mut usize).write(entry as usize);

        extern "C" {
            fn ap_start();
            fn ap_end();
        }
        // copy boot_ap code to 0x6000
        const START_PAGE: u8 = 6;
        let count = ap_end as *const () as usize - ap_start as *const () as usize;
        core::ptr::copy_nonoverlapping(
            ap_start as *const u8,
            phys_to_virt(START_PAGE as usize * 0x1000) as _,
            count,
        );
        // startup
        let apic_region =
            core::slice::from_raw_parts_mut(phys_to_virt(0xfee0_0000) as _, 0x1000 / 4);
        let mut lapic = XAPIC::new(apic_region);
        for &apic_id in ap_ids {
            // set stack
            (phys_to_virt(0x6fe8) as *mut usize).write(stack_fn(apic_id as usize));

            // send IPIs
            let apic = ApicId::XApic(apic_id);
            lapic.ipi_init(apic);
            delay_us(200);
            lapic.ipi_init_deassert();
            delay_us(10000);
            lapic.ipi_startup(apic, START_PAGE);
            delay_us(200);
            lapic.ipi_startup(apic, START_PAGE);
            delay_us(200);

            // wait for startup
            delay_us(10000);
        }
    }
}

fn delay_us(us: u64) {
    use core::arch::x86_64::_rdtsc;
    let start = unsafe { _rdtsc() };
    let freq = super::cpu::cpu_frequency() as u64 * 1_000_000;
    let end = start + freq / 1_000_000 * us;
    while unsafe { _rdtsc() } < end {
        core::hint::spin_loop();
    }
}
