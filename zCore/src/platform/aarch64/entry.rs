use super::adt::Adt;
use super::consts::save_offset;
use super::fdt::parse_fdt;
use alloc::string::String;
use kernel_hal::{AppleMmio, KernelConfig};
use rboot::Aarch64BootInfo;
use spin::Once;

#[cfg(feature = "board-apple")]
core::arch::global_asm!(include_str!("entry.s"));
#[cfg(not(feature = "board-apple"))]
core::arch::global_asm!(include_str!("entry_uefi.s"));
core::arch::global_asm!(include_str!("space.s"));

// FDT Header Magic: 0xd00dfeed in big-endian
const FDT_MAGIC_BE: u32 = 0xd00dfeed;

static BOOT_CMDLINE: Once<String> = Once::new();

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct AppleBootVideo {
    base: u64,
    display: u64,
    stride: u64,
    width: u64,
    height: u64,
    depth: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct AppleBootArgs {
    revision: u16,
    version: u16,
    _pad: u32,
    virt_base: u64,
    phys_base: u64,
    mem_size: u64,
    top_of_kernel_data: u64,
    video: AppleBootVideo,
    machine_type: u32,
    _pad2: u32,
    devtree: u64,
    devtree_size: u32,
    cmdline: [u8; 256],
}

/// Zeroes `.bss`. Required on `board-apple`: the flat `zcore.bin` handed to m1n1
/// contains only file-backed sections, so `.bss` starts as whatever the loader
/// left in that RAM — reading a static before this runs yields garbage
/// (e.g. a `Once`/vtable pointer read as 0x200 and branched to).
#[cfg(feature = "board-apple")]
fn zero_bss() {
    extern "C" {
        static mut sbss: u64;
        static mut ebss: u64;
    }
    unsafe {
        let start = core::ptr::addr_of_mut!(sbss) as *mut u8;
        let end = core::ptr::addr_of_mut!(ebss) as *mut u8;
        core::ptr::write_bytes(start, 0, end as usize - start as usize);
    }
}

/// Entry from `entry.s`.
///
/// * `arg0` — physical address of the FDT/DTB or Apple BootArgs
/// * `offset` — virtual-minus-physical offset of the direct map established by
///   the early page tables. Zero when the loader (rboot) already runs the
///   kernel at its link address.
#[no_mangle]
pub extern "C" fn rust_entry(arg0: usize, offset: usize) -> ! {
    // MUST run before any static is touched.
    #[cfg(feature = "board-apple")]
    zero_bss();

    early_print("HVLOG: [rust_entry] reached successfully!\n");

    let maybe_magic = unsafe {
        let ptr = arg0 as *const u32;
        if !ptr.is_null() && (ptr as usize) >= 0x1000 {
            u32::from_be(*ptr)
        } else {
            0
        }
    };

    if maybe_magic == FDT_MAGIC_BE {
        early_print("HVLOG: [rust_entry] detected FDT\n");
        rust_entry_fdt(arg0, offset);
    } else {
        // Check if arg0 is Apple BootArgs (revision 1/2/3)
        let maybe_ba = unsafe { &*(arg0 as *const AppleBootArgs) };
        if maybe_ba.revision >= 1 && maybe_ba.revision <= 3 && maybe_ba.phys_base >= 0x1000_0000 {
            early_print("HVLOG: [rust_entry] detected Apple BootArgs\n");
            rust_entry_apple_bootargs(maybe_ba, offset);
        } else {
            early_print("HVLOG: [rust_entry] fallback to rboot\n");
            let boot_info = unsafe { &*(arg0 as *const Aarch64BootInfo) };
            rust_entry_rboot(boot_info);
        }
    }
}

/// T6020 (M2 Pro) UART0 physical base.
const UART_PHYS_BASE: usize = 0x39b2_00000;

/// T6020 (M2 Pro) AIC2 physical base.
const AIC_PHYS_BASE: usize = 0x2_8e10_0000;

/// Address `early_putchar` writes through. Starts as the physical address
/// (valid while `entry.s`'s identity map is live) and is switched to the
/// direct-map address once the offset is known, because `vm::init` clears
/// TTBR0 and only maps the UART in the high half.
static EARLY_UART: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(UART_PHYS_BASE);

/// Writes one byte to the Apple SoC UART FIFO, which m1n1 proxies over USB.
/// Only Apple boards have this MMIO window; on other boards (QEMU virt under
/// rboot/UEFI) the register does not exist and probing it aborts.
#[cfg(feature = "board-apple")]
pub fn early_putchar(c: u8) {
    const UTXH: usize = 0x20;
    let base = EARLY_UART.load(core::sync::atomic::Ordering::Relaxed);
    unsafe {
        core::ptr::write_volatile((base + UTXH) as *mut u32, c as u32);
    }
}

#[cfg(not(feature = "board-apple"))]
pub fn early_putchar(_c: u8) {}

pub fn early_print(s: &str) {
    for b in s.bytes() {
        if b == b'\n' {
            early_putchar(b'\r');
        }
        early_putchar(b);
    }
}

/// Copies the boot command line into a static buffer.
///
/// The kernel heap is only initialized inside `primary_main`, so nothing here
/// may allocate: a `String` at this point hits an uninitialized allocator.
fn store_cmdline(src: &[u8]) -> &'static str {
    static mut CMDLINE_BUF: [u8; 256] = [0; 256];
    let len = src.iter().position(|&b| b == 0).unwrap_or(src.len());
    unsafe {
        let buf = &mut *core::ptr::addr_of_mut!(CMDLINE_BUF);
        buf[..len].copy_from_slice(&src[..len]);
        core::str::from_utf8(&buf[..len]).unwrap_or("")
    }
}

fn rust_entry_apple_bootargs(ba: &AppleBootArgs, offset: usize) -> ! {
    // `entry.s` mapped the kernel image at its link address and identity-mapped
    // physical memory, so the direct map is `paddr + offset`.
    save_offset(offset);

    // `entry.s` mapped the UART in the high half too; move early prints there so
    // they survive `vm::init` clearing TTBR0.
    EARLY_UART.store(
        UART_PHYS_BASE + offset,
        core::sync::atomic::Ordering::Relaxed,
    );

    let cmdline = store_cmdline(&ba.cmdline);
    let mut apple = discover_apple_mmio(ba);
    // `ba.video` is iBoot's pre-configured, already-painted boot
    // framebuffer (`/chosen/framebuffer` in the ADT); m1n1 leaves it live
    // across chainload. `video.display == 0` means iBoot itself disabled
    // display output (`-v`/headless boot args), so treat that the same as
    // no framebuffer rather than mapping garbage.
    if ba.video.base != 0 && ba.video.display != 0 && ba.video.width != 0 && ba.video.height != 0 {
        apple.fb_base = ba.video.base as usize;
        apple.fb_stride = ba.video.stride as usize;
        apple.fb_width = ba.video.width as usize;
        apple.fb_height = ba.video.height as usize;
        apple.fb_depth = ba.video.depth as usize;
    }
    early_print("HVLOG: video: base=");
    early_hex(apple.fb_base);
    early_print(" stride=");
    early_hex(apple.fb_stride);
    early_print(" ");
    early_hex(apple.fb_width);
    early_print("x");
    early_hex(apple.fb_height);
    early_print(" depth=");
    early_hex(apple.fb_depth);
    early_print("\nHVLOG: initialising logging & memory...\n");
    let config = KernelConfig {
        cmdline,
        firmware_type: "Apple Silicon (m1n1 BootArgs)",
        uart_base: UART_PHYS_BASE,
        gic_base: if apple.aic_base != 0 {
            apple.aic_base
        } else {
            AIC_PHYS_BASE
        },
        phys_to_virt_offset: offset,
        apple,
    };

    crate::primary_main(config);
    unreachable!()
}

/// Reads the MMIO windows and AIC register layout out of iBoot's ADT.
///
/// `ba.devtree` is a virtual address in iBoot's map, rebased through the
/// BootArgs virt/phys pair exactly as `startup.c` does. The result is read
/// through TTBR0's identity map, which is still live here: `entry.s`'s
/// high-half map only covers the kernel image window and the UART, so
/// `paddr + offset` for arbitrary RAM is a level-2 translation fault.
///
/// Everything here must stay allocation-free: the kernel heap only comes up
/// inside `primary_main`.
fn discover_apple_mmio(ba: &AppleBootArgs) -> AppleMmio {
    let mut out = AppleMmio::default();

    early_print("HVLOG: ba: rev=");
    early_hex(ba.revision as usize);
    early_print(" ver=");
    early_hex(ba.version as usize);
    early_print(" virt=");
    early_hex(ba.virt_base as usize);
    early_print(" phys=");
    early_hex(ba.phys_base as usize);
    early_print(" memsz=");
    early_hex(ba.mem_size as usize);
    early_print(" devtree=");
    early_hex(ba.devtree as usize);
    early_print("/");
    early_hex(ba.devtree_size as usize);
    early_print("\n");

    // `startup.c` rebases the ADT the same way. Validate before dereferencing:
    // a bogus pointer here faults with a translation abort long before any
    // fault handler exists.
    let size = ba.devtree_size as usize;
    let adt_paddr = ba
        .devtree
        .wrapping_sub(ba.virt_base)
        .wrapping_add(ba.phys_base) as usize;
    let plausible = ba.devtree >= ba.virt_base
        && adt_paddr.is_multiple_of(4)
        && (8..=16 << 20).contains(&size)
        && adt_paddr >= ba.phys_base as usize
        && (adt_paddr - ba.phys_base as usize) + size <= ba.mem_size as usize;
    if !plausible {
        early_print("HVLOG: adt: implausible location ");
        early_hex(adt_paddr);
        early_print(", falling back to hardcoded MMIO\n");
        return out;
    }

    let Some(adt) = (unsafe { Adt::new(adt_paddr, size) }) else {
        early_print("HVLOG: adt: bad header, falling back to hardcoded MMIO\n");
        return out;
    };

    if let Some(aic) = adt.path("/arm-io/aic") {
        if let Some((base, size)) = adt.reg(&aic, 0) {
            out.aic_base = base as usize;
            out.aic_size = size as usize;
        }
        let node = aic.offset();
        // The generation MUST come from `compatible`; the version register's
        // low byte reads 8 on T6020, so probing it misdetects AIC2 as AIC1.
        out.aic_version = if adt.is_compatible(node, "aic,3") {
            3
        } else if adt.is_compatible(node, "aic,2") {
            2
        } else if adt.is_compatible(node, "aic,1") {
            1
        } else {
            0
        };
        early_print("HVLOG: adt: aic version=");
        early_hex(out.aic_version as usize);
        early_print("\n");
        let prop = |name: &str| adt.prop_u32(node, name).unwrap_or(0) as usize;
        out.aic_event_offset = prop("aic-iack-offset");
        out.aic_cap0_offset = prop("cap0-offset");
        out.aic_maxnumirq_offset = prop("maxnumirq-offset");
        out.aic_irq_cfg = prop("extint-baseaddress");
        out.aic_cfg_stride = prop("extintrcfg-stride");
        out.aic_mask_set_stride = prop("intmaskset-stride");
        out.aic_mask_clr_stride = prop("intmaskclear-stride");
    }
    if let Some((base, size)) = adt.path_reg("/arm-io/pmgr", 0) {
        out.pmgr_base = base as usize;
        out.pmgr_size = size as usize;
    }
    if let Some(ans) = adt.path("/arm-io/ans") {
        if let Some((base, size)) = adt.reg(&ans, 0) {
            out.ans_base = base as usize;
            out.ans_size = size as usize;
        }
        // reg 3 holds both the NVMe and NVMMU registers on M1-M3 generations,
        // matching `nvme_init()` in m1n1's `src/nvme.c`.
        if let Some((base, size)) = adt.reg(&ans, 3) {
            out.nvme_base = base as usize;
            out.nvme_size = size as usize;
        }
    }
    if let Some((base, _)) = adt.path_reg("/arm-io/ans_mbox", 0) {
        early_print("HVLOG: adt: ans_mbox=");
        early_hex(base as usize);
        early_print("\n");
    }
    if let Some((base, size)) = adt.path_reg("/arm-io/sart-ans", 0) {
        out.sart_base = base as usize;
        out.sart_size = size as usize;
    }
    if let Some(uart) = adt
        .path("/arm-io/uart0")
        .or_else(|| adt.path("/arm-io/uart2"))
    {
        if let Some(irq) = adt.prop_u32(uart.offset(), "interrupts") {
            out.uart_irq = irq as usize;
        }
    }

    early_print("HVLOG: adt: aic=");
    early_hex(out.aic_base);
    early_print("/");
    early_hex(out.aic_size);
    early_print(" iack=");
    early_hex(out.aic_event_offset);
    early_print(" cap0=");
    early_hex(out.aic_cap0_offset);
    early_print(" maxnumirq=");
    early_hex(out.aic_maxnumirq_offset);
    early_print(" extint=");
    early_hex(out.aic_irq_cfg);
    early_print(" strides=");
    early_hex(out.aic_cfg_stride);
    early_print("/");
    early_hex(out.aic_mask_set_stride);
    early_print("/");
    early_hex(out.aic_mask_clr_stride);
    early_print("\nHVLOG: adt: pmgr=");
    early_hex(out.pmgr_base);
    early_print(" ans=");
    early_hex(out.ans_base);
    early_print(" nvme=");
    early_hex(out.nvme_base);
    early_print(" sart=");
    early_hex(out.sart_base);
    early_print("\n");

    out
}

/// Prints `v` as `0x…` through the early UART. No allocation, no `core::fmt`
/// machinery that could touch a lock this early.
fn early_hex(v: usize) {
    early_print("0x");
    if v == 0 {
        early_putchar(b'0');
        return;
    }
    let mut started = false;
    for shift in (0..16).rev() {
        let nibble = ((v >> (shift * 4)) & 0xf) as u8;
        if nibble != 0 {
            started = true;
        }
        if started {
            early_putchar(if nibble < 10 {
                b'0' + nibble
            } else {
                b'a' + nibble - 10
            });
        }
    }
}

fn rust_entry_fdt(dtb_paddr: usize, offset: usize) -> ! {
    save_offset(offset);

    // Parse FDT directly
    let parsed_fdt = parse_fdt(dtb_paddr);

    let cmdline: &'static str = if let Some(fdt) = &parsed_fdt {
        if let Some(cmd) = &fdt.cmdline {
            BOOT_CMDLINE.call_once(|| cmd.clone());
            BOOT_CMDLINE.get().map_or("", |s| s.as_str())
        } else {
            ""
        }
    } else {
        ""
    };

    let uart_base = parsed_fdt
        .as_ref()
        .and_then(|f| f.uart_base)
        .unwrap_or(0x39b2_00000); // Default fallback M2 Pro UART0

    let gic_base = parsed_fdt
        .as_ref()
        .and_then(|f| f.gic_base)
        .unwrap_or(0x0800_0000);

    let aic_base = parsed_fdt.as_ref().and_then(|f| f.aic_base).unwrap_or(0);

    let _aic_event_base = parsed_fdt
        .as_ref()
        .and_then(|f| f.aic_event_base)
        .unwrap_or(0);

    let is_apple = aic_base != 0
        || parsed_fdt
            .as_ref()
            .is_some_and(|f| f.uart_base.is_some() && aic_base != 0)
        || uart_base == 0x39b2_00000;

    let firmware_type = if is_apple {
        "Apple Silicon / Asahi (m1n1)"
    } else {
        "FDT (Linux AArch64)"
    };

    // The FDT `apple,aic2` node carries the two register windows but none of
    // the layout properties iBoot's ADT has; leave those zero so the driver
    // derives them from MAXNUMIRQ.
    let mut apple = AppleMmio::default();
    if is_apple && aic_base != 0 {
        apple.aic_base = aic_base;
        apple.aic_event_offset = _aic_event_base.saturating_sub(aic_base);
        apple.aic_version = parsed_fdt.as_ref().and_then(|f| f.aic_version).unwrap_or(0);
    }

    let config = KernelConfig {
        cmdline,
        firmware_type,
        uart_base,
        gic_base: if is_apple { aic_base } else { gic_base },
        phys_to_virt_offset: 0,
        apple,
    };

    crate::primary_main(config);
    unreachable!()
}

fn rust_entry_rboot(boot_info: &'static Aarch64BootInfo) -> ! {
    let config = KernelConfig {
        cmdline: boot_info.cmdline,
        firmware_type: boot_info.firmware_type,
        uart_base: boot_info.uart_base,
        gic_base: boot_info.gic_base,
        phys_to_virt_offset: boot_info.offset,
        apple: AppleMmio::default(),
    };
    save_offset(boot_info.offset);
    crate::primary_main(config);
    unreachable!()
}
