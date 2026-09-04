//! Hypercall trampoline into m1n1's resident hypervisor (`run_guest.py`).
//!
//! Apple Silicon gates the ANS/NVMe MMIO window (`/arm-io/ans`) so that only
//! its privileged owner may write it — under `run_guest.py` that owner is
//! m1n1's own EL2 firmware, not the EL1 guest. Every read we've tried
//! through that window succeeds (`cpu_ctrl`, mailbox `CONTROL`, NVMe
//! `BOOT_STATUS`); every write NAKs with an asynchronous SError
//! (`L2C_ERR_STS` ACCESS_FAULT), regardless of which register.
//!
//! m1n1's HV already has a generic `brk #0x4242` hypercall trap
//! (`handle_brk` in `proxyclient/m1n1/hv/__init__.py`, dispatching by `x0`
//! through `hv.add_hvcall`). A companion script (`nvme_proxy.py`, loaded via
//! `run_guest.py -m`) registers handlers that perform the actual MMIO access
//! from the host side over the m1n1 proxy protocol (`p.write32`/`p.read32`,
//! ...), which executes at m1n1's own privilege and is not subject to the
//! same gate. This module is the guest-side half of that trampoline.
//!
//! Only usable under `run_guest.py` with `nvme_proxy.py` loaded; without a
//! registered handler the HV logs "Undefined HV call" and resumes the guest
//! without advancing `elr`, which hangs the calling core. There is no
//! fallback path — this is exclusively for the HV-supervised boot flow.

/// Must match `NVME_HVCALL_READ` in `nvme_proxy.py`.
const HVCALL_NVME_READ: u64 = 0x4e56_0001;
/// Must match `NVME_HVCALL_WRITE` in `nvme_proxy.py`.
const HVCALL_NVME_WRITE: u64 = 0x4e56_0002;

/// Traps to the HV with `x0`/`x1`/`x2`/`x3` = `id`/`a1`/`a2`/`a3`, returning
/// the handler's `x0`. `brk #0x4242` is m1n1's fixed hypercall opcode/ISS;
/// changing it requires updating `handle_brk`'s `iss != 0x4242` check too.
///
/// `zcore-drivers` also builds for the libos/x86_64 host target (where this
/// crate's aarch64-only drivers are unused but still typechecked), so the
/// `aarch64`-only inline asm is cfg-gated; other targets never call this.
#[inline(always)]
#[cfg(target_arch = "aarch64")]
unsafe fn hvcall4(id: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "mov x0, {id}",
            "mov x1, {a1}",
            "mov x2, {a2}",
            "mov x3, {a3}",
            "brk #0x4242",
            "mov {ret}, x0",
            id = in(reg) id,
            a1 = in(reg) a1,
            a2 = in(reg) a2,
            a3 = in(reg) a3,
            ret = out(reg) ret,
            out("x0") _, out("x1") _, out("x2") _, out("x3") _,
            options(nostack),
        );
    }
    ret
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn hvcall4(_id: u64, _a1: u64, _a2: u64, _a3: u64) -> u64 {
    unimplemented!("m1n1 HV hypercalls are aarch64-only")
}

/// Writes `val` to physical/IPA address `addr` via the HV, width in bytes
/// (1, 2, 4, or 8).
///
/// # Safety
///
/// `addr` must be a valid physical/IPA MMIO register address for the given
/// `width`, and the caller must not rely on ordering with respect to other
/// memory accesses beyond what the trap's implicit context switch provides
/// (no memory barrier is issued around the `brk`).
pub unsafe fn hv_write(addr: usize, width: u32, val: u64) {
    unsafe {
        hvcall4(HVCALL_NVME_WRITE, addr as u64, width as u64, val);
    }
}

/// Reads from physical/IPA address `addr` via the HV, width in bytes
/// (1, 2, 4, or 8).
///
/// # Safety
///
/// `addr` must be a valid physical/IPA MMIO register address for the given
/// `width`.
pub unsafe fn hv_read(addr: usize, width: u32) -> u64 {
    unsafe { hvcall4(HVCALL_NVME_READ, addr as u64, width as u64, 0) }
}
