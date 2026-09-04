//! Apple ANS (Apple NVMe Storage / ANS2) Driver
//!
//! Controls Apple Silicon internal NVMe storage via ANS2 linear queues
//! and NVMMU (Task Control Blocks). Targets T6020 (M2 Pro); ANS2 itself is
//! an RTKit coprocessor and must be booted over its mailbox (see
//! [`crate::soc::apple_rtkit`]) with a SART DMA allow-list configured
//! before any of its registers below `APPLE_ANS_BOOT_STATUS` are live.

use core::ptr::read_volatile;
use core::sync::atomic::{fence, Ordering};

use crate::scheme::{BlockScheme, Scheme};
use crate::soc::{hvcall, AppleMailbox, AppleRtKit, AppleSart};
use crate::sync::Mutex;
use crate::{DeviceError, DeviceResult};

use super::nvme_queue::{Provider, ProviderImpl, PAGE_SIZE};

// Standard NVMe registers
pub const NVME_REG_CAP: usize = 0x0000;
pub const NVME_REG_VS: usize = 0x0008;
pub const NVME_REG_CC: usize = 0x0014;
pub const NVME_REG_CSTS: usize = 0x001c;
pub const NVME_REG_AQA: usize = 0x0024;
pub const NVME_REG_ASQ: usize = 0x0028;
pub const NVME_REG_ACQ: usize = 0x0030;

pub const NVME_CC_EN: u32 = 1 << 0;
pub const NVME_CC_CSS_NVM: u32 = 0 << 4;
pub const NVME_CC_SHN_NONE: u32 = 0 << 14;
pub const NVME_CC_SHN_NORMAL: u32 = 1 << 14;
pub const NVME_CC_SHN_MASK: u32 = 3 << 14;
pub const NVME_CC_IOSQES: u32 = 6 << 16;
pub const NVME_CC_IOCQES: u32 = 4 << 20;

pub const NVME_CSTS_RDY: u32 = 1 << 0;
pub const NVME_CSTS_SHST_MASK: u32 = 3 << 2;
pub const NVME_CSTS_SHST_DONE: u32 = 2 << 2;

// Apple ANS specific registers
pub const APPLE_ANS_ACQ_DB: usize = 0x1004;
pub const APPLE_ANS_IOCQ_DB: usize = 0x100c;
pub const APPLE_ANS_IOQ_CMDS: usize = 0x1200;
pub const APPLE_ANS_IOQ_CQES: usize = 0x1208;
pub const APPLE_ANS_MAX_PEND_CMDS_CTRL: usize = 0x1210;
pub const APPLE_ANS_BOOT_STATUS: usize = 0x1300;
/// ASC CPU control, in the secondary (`ans`) window — not the NVMe window.
pub const APPLE_ANS_COPROC_CPU_CONTROL: usize = 0x44;
pub const APPLE_ANS_BOOT_STATUS_OK: u32 = 0xde71ce55;

pub const APPLE_ANS_LINEAR_SQ_CTRL: usize = 0x24908;
pub const APPLE_ANS_LINEAR_SQ_EN: u32 = 1 << 0;
pub const APPLE_ANS_LINEAR_ASQ_DB: usize = 0x2490c;
pub const APPLE_ANS_LINEAR_IOSQ_DB: usize = 0x24910;

// Apple NVMMU registers
pub const APPLE_NVMMU_NUM_TCBS: usize = 0x28100;
pub const APPLE_NVMMU_ASQ_TCB_BASE: usize = 0x28108;
pub const APPLE_NVMMU_IOSQ_TCB_BASE: usize = 0x28110;
pub const APPLE_NVMMU_TCB_INVAL: usize = 0x28118;
pub const APPLE_NVMMU_TCB_STAT: usize = 0x28120;

// Command Opcodes
pub const NVME_ADMIN_CMD_DELETE_SQ: u8 = 0x00;
pub const NVME_ADMIN_CMD_CREATE_SQ: u8 = 0x01;
pub const NVME_ADMIN_CMD_DELETE_CQ: u8 = 0x04;
pub const NVME_ADMIN_CMD_CREATE_CQ: u8 = 0x05;
pub const NVME_ADMIN_CMD_IDENTIFY: u8 = 0x06;

pub const NVME_CMD_FLUSH: u8 = 0x00;
pub const NVME_CMD_WRITE: u8 = 0x01;
pub const NVME_CMD_READ: u8 = 0x02;

pub const NVMMU_TCB_DMA_FROM_DEVICE: u8 = 1 << 0;
pub const NVMMU_TCB_DMA_TO_DEVICE: u8 = 1 << 1;

pub const ANS_QUEUE_DEPTH: usize = 64;
/// Fixed admin queue depth. Unlike the IO queue (sized up to
/// [`ANS_QUEUE_DEPTH`]), Apple ANS firmware always uses a 2-entry admin
/// queue (`APPLE_NVME_AQ_DEPTH` in Asahi Linux's `apple.c`) regardless of
/// what a driver requests; AQA must match or the controller rejects it.
pub const ADMIN_QUEUE_DEPTH: usize = 2;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NvmeCommand {
    pub opcode: u8,
    pub flags: u8,
    pub tag: u8,
    pub rsvd: u8,
    pub nsid: u32,
    pub cdw2: u32,
    pub cdw3: u32,
    pub metadata: u64,
    pub prp1: u64,
    pub prp2: u64,
    pub cdw10: u32,
    pub cdw11: u32,
    pub cdw12: u32,
    pub cdw13: u32,
    pub cdw14: u32,
    pub cdw15: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NvmeCompletion {
    pub result: u64,
    pub rsvd: u32,
    pub tag: u16,
    pub status: u16,
}

#[repr(C, align(128))]
#[derive(Clone, Copy)]
pub struct AppleNvmmuTcb {
    pub opcode: u8,
    pub dma_flags: u8,
    pub slot_id: u8,
    pub unk0: u8,
    pub length: u32,
    pub unk1: [u64; 2],
    pub prp1: u64,
    pub prp2: u64,
    pub unk2: [u64; 2],
    pub aes_iv: [u8; 8],
    pub aes_unk: [u8; 64],
}

impl Default for AppleNvmmuTcb {
    fn default() -> Self {
        Self {
            opcode: 0,
            dma_flags: 0,
            slot_id: 0,
            unk0: 0,
            length: 0,
            unk1: [0; 2],
            prp1: 0,
            prp2: 0,
            unk2: [0; 2],
            aes_iv: [0; 8],
            aes_unk: [0; 64],
        }
    }
}

pub struct AppleAnsQueue {
    tcbs_va: usize,
    tcbs_pa: usize,
    cmds_va: usize,
    cmds_pa: usize,
    cqes_va: usize,
    cqes_pa: usize,
    cq_head: u8,
    cq_phase: u8,
    is_admin: bool,
}

impl AppleAnsQueue {
    pub fn new(is_admin: bool) -> Self {
        // Allocate DMA buffers for TCBs, commands, and completion entries
        let (tcbs_va, tcbs_pa) = ProviderImpl::alloc_dma(PAGE_SIZE);
        let (cmds_va, cmds_pa) = ProviderImpl::alloc_dma(PAGE_SIZE);
        let (cqes_va, cqes_pa) = ProviderImpl::alloc_dma(PAGE_SIZE);

        unsafe {
            core::ptr::write_bytes(tcbs_va as *mut u8, 0, PAGE_SIZE);
            core::ptr::write_bytes(cmds_va as *mut u8, 0, PAGE_SIZE);
            core::ptr::write_bytes(cqes_va as *mut u8, 0, PAGE_SIZE);
        }

        Self {
            tcbs_va,
            tcbs_pa,
            cmds_va,
            cmds_pa,
            cqes_va,
            cqes_pa,
            cq_head: 0,
            cq_phase: 1,
            is_admin,
        }
    }

    pub fn exec_command(
        &mut self,
        phys_nvme_base: usize,
        cmd: &NvmeCommand,
        rtkit: &mut AppleRtKit,
    ) -> DeviceResult<u64> {
        let tag: u8 = 0;
        let mut queue_cmd = *cmd;
        queue_cmd.tag = tag;

        // Configure NVMMU TCB for this command tag
        let dma_flags = if queue_cmd.prp1 == 0 {
            0
        } else if queue_cmd.opcode == NVME_CMD_WRITE {
            NVMMU_TCB_DMA_TO_DEVICE
        } else {
            NVMMU_TCB_DMA_FROM_DEVICE
        };
        let tcb = AppleNvmmuTcb {
            opcode: 0,
            dma_flags,
            slot_id: tag,
            length: queue_cmd.cdw12,
            prp1: queue_cmd.prp1,
            prp2: queue_cmd.prp2,
            ..Default::default()
        };
        unsafe {
            let cmd_ptr = (self.cmds_va as *mut NvmeCommand).add(tag as usize);
            let tcb_ptr = (self.tcbs_va as *mut AppleNvmmuTcb).add(tag as usize);
            core::ptr::write_volatile(cmd_ptr, queue_cmd);
            core::ptr::write_volatile(tcb_ptr, tcb);
        }

        fence(Ordering::SeqCst);

        // Ring Linear SQ submission doorbell. Device MMIO, not DMA memory —
        // proxied through the HV (see `crate::soc::hvcall`).
        let sq_db_off = if self.is_admin {
            APPLE_ANS_LINEAR_ASQ_DB
        } else {
            APPLE_ANS_LINEAR_IOSQ_DB
        };
        unsafe {
            hvcall::hv_write(phys_nvme_base + sq_db_off, 4, tag as u64);
        }

        // Poll for completion at cq_head
        let cq_doorbell_off = if self.is_admin {
            APPLE_ANS_ACQ_DB
        } else {
            APPLE_ANS_IOCQ_DB
        };

        let mut spins = 0usize;
        const TIMEOUT_SPINS: usize = 50_000_000;

        loop {
            if !rtkit.poll() {
                error!("Apple ANS RTKit coprocessor crashed while a command was in flight");
                return Err(DeviceError::IoError);
            }
            fence(Ordering::SeqCst);
            let cqe_ptr =
                unsafe { (self.cqes_va as *const NvmeCompletion).add(self.cq_head as usize) };
            let cqe = unsafe { read_volatile(cqe_ptr) };

            let phase = (cqe.status & 1) as u8;
            if phase == self.cq_phase {
                // Command completed!

                // Invalidate this tag's NVMMU TCB entry — required after
                // every completion, matching m1n1's nvme_exec_command()
                // and Linux's apple_nvmmu_inval(): the coprocessor tracks
                // TCB slots as busy until told otherwise, and reusing a
                // tag without invalidating it first (e.g. the admin
                // queue's single tag 0 for back-to-back CREATE_CQ then
                // CREATE_SQ) crashes ANS2 rather than erroring.
                unsafe {
                    hvcall::hv_write(phys_nvme_base + APPLE_NVMMU_TCB_INVAL, 4, cqe.tag as u64);
                }
                let inval_stat =
                    unsafe { hvcall::hv_read(phys_nvme_base + APPLE_NVMMU_TCB_STAT, 4) as u32 };
                if inval_stat != 0 {
                    error!(
                        "Apple ANS NVMe: NVMMU TCB invalidation for tag {} failed (stat={:#x})",
                        cqe.tag, inval_stat
                    );
                }

                let depth = if self.is_admin {
                    ADMIN_QUEUE_DEPTH
                } else {
                    ANS_QUEUE_DEPTH
                };
                self.cq_head = (self.cq_head + 1) % (depth as u8);
                if self.cq_head == 0 {
                    self.cq_phase ^= 1;
                }

                // Ring CQ doorbell to acknowledge completion
                unsafe {
                    hvcall::hv_write(phys_nvme_base + cq_doorbell_off, 4, self.cq_head as u64);
                }

                let status_code = (cqe.status >> 1) & 0x7fff;
                if status_code == 0 {
                    return Ok(cqe.result);
                } else {
                    error!("Apple ANS NVMe command error: status={:#x}", status_code);
                    return Err(DeviceError::IoError);
                }
            }

            spins += 1;
            if spins > TIMEOUT_SPINS {
                error!(
                    "Apple ANS NVMe command timed out (opcode {:#x})",
                    cmd.opcode
                );
                return Err(DeviceError::NotReady);
            }
            core::hint::spin_loop();
        }
    }
}

impl Drop for AppleAnsQueue {
    fn drop(&mut self) {
        ProviderImpl::dealloc_dma(self.tcbs_va, PAGE_SIZE);
        ProviderImpl::dealloc_dma(self.cmds_va, PAGE_SIZE);
        ProviderImpl::dealloc_dma(self.cqes_va, PAGE_SIZE);
    }
}

pub struct AppleAnsNvme {
    nvme_base: usize,
    /// Physical address of `nvme_base`, used for register writes (see
    /// [`crate::soc::hvcall`] — reads still use `nvme_base` directly).
    phys_nvme_base: usize,
    #[allow(unused)]
    ans_base: usize,
    rtkit: Mutex<AppleRtKit>,
    admin_q: Mutex<AppleAnsQueue>,
    io_q: Mutex<AppleAnsQueue>,
    block_count: usize,
    block_size: usize,
}

/// Spins allowed while waiting for `APPLE_ANS_BOOT_STATUS` to report a
/// successful firmware boot.
const BOOT_STATUS_SPIN_TIMEOUT: usize = 20_000_000;

impl AppleAnsNvme {
    /// Initialize the Apple ANS NVMe driver from its MMIO base addresses.
    ///
    /// `nvme_base`/`phys_nvme_base`: virtual/physical address of the primary
    /// NVMe controller MMIO window (0x40000 bytes).
    /// `ans_base`/`phys_ans_base`: virtual/physical address of the secondary
    /// ANS/ASC-CPU MMIO window (0x4000 bytes).
    /// `mbox_base`/`phys_mbox_base`: virtual/physical address of the ANS2
    /// mailbox (`ans_mbox`) MMIO window.
    /// `sart_base`/`phys_sart_base`: virtual/physical address of the SART
    /// DMA allow-list MMIO window. Reads use `sart_base` directly; writes
    /// go through the same hypercall proxy as this driver's own registers
    /// (see [`crate::soc::hvcall`]).
    ///
    /// Every physical address is required: on T6020 under m1n1's resident
    /// hypervisor (`run_guest.py`), writes to this MMIO region from the EL1
    /// guest NAK with an asynchronous SError regardless of register; only
    /// the HV's own EL2 access succeeds. Register writes are proxied there
    /// via a `brk`-based hypercall (see [`crate::soc::hvcall`]), which needs
    /// the physical address the host-side handler operates on.
    /// `reset_ans` pulses the ANS2 power domain's PMGR reset (the platform
    /// owns the PMGR window, so the caller supplies this). It is always
    /// invoked: this driver never adopts a coprocessor another owner already
    /// booted, matching `apple_nvme_reset_work()` in Asahi Linux's
    /// `apple.c`, which shuts RTKit down, clears `COPROC_CPU_CONTROL.RUN`,
    /// resets the die and re-boots the firmware whenever it finds ANS
    /// running. Adopting m1n1's live ANS instead leaves firmware state
    /// (its own IOCQ/IOSQ 1) that a `CC.EN` cycle does not reap, and ANS2
    /// panics rather than erroring when asked to re-create those queues.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        nvme_base: usize,
        phys_nvme_base: usize,
        ans_base: usize,
        phys_ans_base: usize,
        mbox_base: usize,
        phys_mbox_base: usize,
        sart_base: usize,
        phys_sart_base: usize,
        reset_ans: &dyn Fn(),
    ) -> DeviceResult<Self> {
        // Park the ASC CPU, reset the power domain, and let `boot()` below
        // start it again from scratch: `ASC_CPU_CONTROL.RUN` clear on entry
        // is what makes it run the full HELLO/EPMAP handshake.
        unsafe { hvcall::hv_write(phys_ans_base + APPLE_ANS_COPROC_CPU_CONTROL, 4, 0) };
        reset_ans();

        let mailbox = AppleMailbox::new(mbox_base, phys_mbox_base);
        let sart = AppleSart::new(sart_base, phys_sart_base);
        let mut rtkit = AppleRtKit::new(mailbox, sart);
        if !rtkit.boot(ans_base, phys_ans_base) {
            error!("Apple ANS RTKit boot failed");
            return Err(DeviceError::NotReady);
        }

        // Wait for ANS2's own firmware to finish booting before touching any
        // NVMe register; the RTKit handshake only brought up the IOP shell,
        // not the NVMe personality running on top of it.
        let mut spins = 0;
        loop {
            let status =
                unsafe { read_volatile((nvme_base + APPLE_ANS_BOOT_STATUS) as *const u32) };
            if status == APPLE_ANS_BOOT_STATUS_OK {
                break;
            }
            rtkit.poll();
            spins += 1;
            if spins > BOOT_STATUS_SPIN_TIMEOUT {
                error!("Apple ANS did not boot correctly (status {:#x})", status);
                return Err(DeviceError::NotReady);
            }
            core::hint::spin_loop();
        }

        let mut dev = Self {
            nvme_base,
            phys_nvme_base,
            ans_base,
            rtkit: Mutex::new(rtkit),
            admin_q: Mutex::new(AppleAnsQueue::new(true)),
            io_q: Mutex::new(AppleAnsQueue::new(false)),
            block_count: 0,
            block_size: 4096,
        };

        // Whitelist the queues' own DMA memory (TCB arrays, command rings,
        // completion rings) for ANS2 to touch — permanent for the driver's
        // lifetime, unlike the transient per-I/O data buffers allow-listed
        // (and freed) around each `exec_command` call in `identify_namespace`
        // / `read_block` / `write_block`.
        {
            let admin_q = dev.admin_q.lock();
            let io_q = dev.io_q.lock();
            let mut rtkit_guard = dev.rtkit.lock();
            for pa in [
                admin_q.tcbs_pa,
                admin_q.cmds_pa,
                admin_q.cqes_pa,
                io_q.tcbs_pa,
                io_q.cmds_pa,
                io_q.cqes_pa,
            ] {
                if !rtkit_guard.sart_add_allowed_region(pa, PAGE_SIZE) {
                    error!(
                        "Apple ANS NVMe: SART has no free entry for queue buffer at {:#x}",
                        pa
                    );
                    return Err(DeviceError::NotReady);
                }
            }
        }

        dev.init_controller()?;
        Ok(dev)
    }

    fn write_reg(&self, offset: usize, val: u32) {
        unsafe { hvcall::hv_write(self.phys_nvme_base + offset, 4, val as u64) };
    }

    fn read_reg(&self, offset: usize) -> u32 {
        unsafe { read_volatile((self.nvme_base + offset) as *const u32) }
    }

    fn write_reg64(&self, offset: usize, val: u64) {
        unsafe { hvcall::hv_write(self.phys_nvme_base + offset, 8, val) };
    }

    fn init_controller(&mut self) -> DeviceResult {
        let cc = self.read_reg(NVME_REG_CC);
        let csts = self.read_reg(NVME_REG_CSTS);
        info!("Apple ANS NVMe: pre-init CC={:#x} CSTS={:#x}", cc, csts);

        // The controller may already be enabled and running (m1n1's own
        // `nvme_init()` boots and enables it during chainload). Reprogramming
        // AQA/ASQ/ACQ while it's still live nacks on T6020 (L2C
        // ACCESS_FAULT) unless we go through a proper NVMe shutdown
        // notification first, exactly as m1n1's `nvme_ctrl_shutdown()` +
        // `nvme_ctrl_disable()` do — not just clearing CC.EN.
        if (cc & NVME_CC_EN) != 0 {
            self.write_reg(NVME_REG_CC, (cc & !NVME_CC_SHN_MASK) | NVME_CC_SHN_NORMAL);
            let mut spins = 0;
            while (self.read_reg(NVME_REG_CSTS) & NVME_CSTS_SHST_MASK) != NVME_CSTS_SHST_DONE {
                spins += 1;
                if spins > 5_000_000 {
                    error!("Apple ANS NVMe: timed out waiting for CSTS.SHST=Done");
                    break;
                }
                core::hint::spin_loop();
            }

            let cc = self.read_reg(NVME_REG_CC);
            self.write_reg(NVME_REG_CC, cc & !NVME_CC_EN);
            let mut spins = 0;
            while (self.read_reg(NVME_REG_CSTS) & NVME_CSTS_RDY) != 0 {
                spins += 1;
                if spins > 5_000_000 {
                    return Err(DeviceError::NotReady);
                }
                core::hint::spin_loop();
            }
        }

        let admin_q = self.admin_q.lock();

        // Configure Admin Queue Attributes (AQA). Unlike the IO queue's
        // qsize field below, the admin queue depth is fixed at
        // `ADMIN_QUEUE_DEPTH` regardless of `ANS_QUEUE_DEPTH` — matching
        // `apple_nvme_enable_ctrl()` in Asahi Linux's `apple.c`.
        let aqa = (((ADMIN_QUEUE_DEPTH - 1) as u32) << 16) | ((ADMIN_QUEUE_DEPTH - 1) as u32);
        self.write_reg(NVME_REG_AQA, aqa);

        // Configure ASQ and ACQ physical base addresses
        self.write_reg64(NVME_REG_ASQ, admin_q.cmds_pa as u64);
        self.write_reg64(NVME_REG_ACQ, admin_q.cqes_pa as u64);

        // Enable ANS Linear Submission Queue Mode
        self.write_reg(APPLE_ANS_LINEAR_SQ_CTRL, APPLE_ANS_LINEAR_SQ_EN);
        // Allow as many pending commands as possible for both queues (not
        // depth - 1: this is a count, not a 0's-based queue size field).
        self.write_reg(
            APPLE_ANS_MAX_PEND_CMDS_CTRL,
            (ANS_QUEUE_DEPTH as u32) | ((ANS_QUEUE_DEPTH as u32) << 16),
        );
        self.write_reg(APPLE_NVMMU_NUM_TCBS, (ANS_QUEUE_DEPTH - 1) as u32);

        // Configure NVMMU TCB Bases for Admin and IO Queues
        let io_q = self.io_q.lock();
        self.write_reg64(APPLE_NVMMU_ASQ_TCB_BASE, admin_q.tcbs_pa as u64);
        self.write_reg64(APPLE_NVMMU_IOSQ_TCB_BASE, io_q.tcbs_pa as u64);

        drop(admin_q);
        drop(io_q);

        // Enable controller in NVMe CC
        let cc_val =
            NVME_CC_EN | NVME_CC_CSS_NVM | NVME_CC_SHN_NONE | NVME_CC_IOSQES | NVME_CC_IOCQES;
        self.write_reg(NVME_REG_CC, cc_val);

        // Wait for controller ready (CSTS.RDY == 1)
        let mut spins = 0;
        while (self.read_reg(NVME_REG_CSTS) & NVME_CSTS_RDY) == 0 {
            spins += 1;
            if spins > 10_000_000 {
                error!("Apple ANS NVMe controller failed to become ready");
                return Err(DeviceError::NotReady);
            }
            core::hint::spin_loop();
        }

        info!("Apple ANS NVMe controller initialized successfully");

        // Create I/O Completion Queue (CQID = 1)
        let io_q = self.io_q.lock();
        let create_cq_cmd = NvmeCommand {
            opcode: NVME_ADMIN_CMD_CREATE_CQ,
            prp1: io_q.cqes_pa as u64,
            cdw10: (((ANS_QUEUE_DEPTH - 1) as u32) << 16) | 1, // size and CQID = 1
            cdw11: 3, // Physically contiguous (bit0) + interrupts enabled (bit1)
            ..Default::default()
        };
        self.admin_q.lock().exec_command(
            self.phys_nvme_base,
            &create_cq_cmd,
            &mut self.rtkit.lock(),
        )?;

        // Create I/O Submission Queue (SQID = 1)
        let create_sq_cmd = NvmeCommand {
            opcode: NVME_ADMIN_CMD_CREATE_SQ,
            prp1: io_q.cmds_pa as u64,
            cdw10: (((ANS_QUEUE_DEPTH - 1) as u32) << 16) | 1, // size and SQID = 1
            cdw11: (1 << 16) | 1,                              // CQID = 1, physically contiguous
            ..Default::default()
        };
        self.admin_q.lock().exec_command(
            self.phys_nvme_base,
            &create_sq_cmd,
            &mut self.rtkit.lock(),
        )?;

        drop(io_q);

        // Identify Namespace 1 to read disk capacity and sector size
        self.identify_namespace(1)?;

        Ok(())
    }

    fn identify_namespace(&mut self, nsid: u32) -> DeviceResult {
        let (ident_va, ident_pa) = ProviderImpl::alloc_dma(PAGE_SIZE);
        unsafe {
            core::ptr::write_bytes(ident_va as *mut u8, 0, PAGE_SIZE);
        }

        if !self
            .rtkit
            .lock()
            .sart_add_allowed_region(ident_pa, PAGE_SIZE)
        {
            ProviderImpl::dealloc_dma(ident_va, PAGE_SIZE);
            error!("Apple ANS NVMe: SART has no free entry for IDENTIFY buffer");
            return Err(DeviceError::NotReady);
        }

        let cmd = NvmeCommand {
            opcode: NVME_ADMIN_CMD_IDENTIFY,
            nsid,
            prp1: ident_pa as u64,
            cdw10: 0, // Identify Namespace (CNS = 0)
            cdw12: 0, // reserved for IDENTIFY; must stay 0 (real hw rejects nonzero here)
            ..Default::default()
        };

        let res =
            self.admin_q
                .lock()
                .exec_command(self.phys_nvme_base, &cmd, &mut self.rtkit.lock());
        self.rtkit
            .lock()
            .sart_remove_allowed_region(ident_pa, PAGE_SIZE);
        if let Err(e) = res {
            ProviderImpl::dealloc_dma(ident_va, PAGE_SIZE);
            return Err(e);
        }

        // Parse NSZE (Namespace Size in blocks) and LBA format
        let ident_bytes = unsafe { core::slice::from_raw_parts(ident_va as *const u8, PAGE_SIZE) };
        let mut nsze_bytes = [0u8; 8];
        nsze_bytes.copy_from_slice(&ident_bytes[0..8]);
        let nsze = u64::from_le_bytes(nsze_bytes);
        let flbas = ident_bytes[26];
        let lba_idx = (flbas & 0x0f) as usize;
        let lbaf_offset = 128 + lba_idx * 4;
        let lba_shift = ident_bytes[lbaf_offset + 2];
        let block_size = if (9..=16).contains(&lba_shift) {
            1 << lba_shift
        } else {
            4096
        };

        // Standardize block count to 512-byte sectors for BlockScheme
        let sectors_per_lba = block_size / 512;
        self.block_size = block_size;
        self.block_count = (nsze as usize) * sectors_per_lba;

        error!(
            "Apple ANS NVMe Namespace {}: {} LBA blocks ({} bytes/LBA), total {} 512-byte sectors ({} MB)",
            nsid,
            nsze,
            block_size,
            self.block_count,
            (self.block_count * 512) / (1024 * 1024)
        );

        ProviderImpl::dealloc_dma(ident_va, PAGE_SIZE);
        Ok(())
    }
}

impl Scheme for AppleAnsNvme {
    fn name(&self) -> &str {
        "apple-ans-nvme"
    }
}

impl BlockScheme for AppleAnsNvme {
    fn block_count(&self) -> usize {
        self.block_count
    }

    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> DeviceResult {
        let (dma_va, dma_pa) = ProviderImpl::alloc_dma(PAGE_SIZE);

        if !self.rtkit.lock().sart_add_allowed_region(dma_pa, PAGE_SIZE) {
            ProviderImpl::dealloc_dma(dma_va, PAGE_SIZE);
            error!("Apple ANS NVMe: SART has no free entry for read buffer");
            return Err(DeviceError::NotReady);
        }

        let sectors_per_lba = self.block_size / 512;
        let lba = (block_id / sectors_per_lba) as u64;
        let lba_offset = (block_id % sectors_per_lba) * 512;

        let cmd = NvmeCommand {
            opcode: NVME_CMD_READ,
            nsid: 1,
            prp1: dma_pa as u64,
            cdw10: lba as u32,
            cdw11: (lba >> 32) as u32,
            cdw12: 0, // 1 LBA block (0-based)
            ..Default::default()
        };
        let res = self
            .io_q
            .lock()
            .exec_command(self.phys_nvme_base, &cmd, &mut self.rtkit.lock());
        self.rtkit
            .lock()
            .sart_remove_allowed_region(dma_pa, PAGE_SIZE);
        if res.is_ok() {
            let src = unsafe {
                core::slice::from_raw_parts((dma_va + lba_offset) as *const u8, buf.len().min(512))
            };
            buf[..src.len()].copy_from_slice(src);
        }

        ProviderImpl::dealloc_dma(dma_va, PAGE_SIZE);
        res.map(|_| ())
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) -> DeviceResult {
        let (dma_va, dma_pa) = ProviderImpl::alloc_dma(PAGE_SIZE);

        if !self.rtkit.lock().sart_add_allowed_region(dma_pa, PAGE_SIZE) {
            ProviderImpl::dealloc_dma(dma_va, PAGE_SIZE);
            error!("Apple ANS NVMe: SART has no free entry for write buffer");
            return Err(DeviceError::NotReady);
        }

        let sectors_per_lba = self.block_size / 512;
        let lba = (block_id / sectors_per_lba) as u64;
        let lba_offset = (block_id % sectors_per_lba) * 512;

        // If sector size is larger than 512, read existing block first (read-modify-write)
        if self.block_size > 512 {
            let read_cmd = NvmeCommand {
                opcode: NVME_CMD_READ,
                nsid: 1,
                prp1: dma_pa as u64,
                cdw10: lba as u32,
                cdw11: (lba >> 32) as u32,
                cdw12: 0,
                ..Default::default()
            };
            let _ = self.io_q.lock().exec_command(
                self.phys_nvme_base,
                &read_cmd,
                &mut self.rtkit.lock(),
            );
        }

        unsafe {
            let dst = core::slice::from_raw_parts_mut(
                (dma_va + lba_offset) as *mut u8,
                buf.len().min(512),
            );
            dst.copy_from_slice(&buf[..dst.len()]);
        }

        let cmd = NvmeCommand {
            opcode: NVME_CMD_WRITE,
            nsid: 1,
            prp1: dma_pa as u64,
            cdw10: lba as u32,
            cdw11: (lba >> 32) as u32,
            cdw12: 0, // 1 LBA block
            ..Default::default()
        };
        let res = self
            .io_q
            .lock()
            .exec_command(self.phys_nvme_base, &cmd, &mut self.rtkit.lock());
        self.rtkit
            .lock()
            .sart_remove_allowed_region(dma_pa, PAGE_SIZE);
        ProviderImpl::dealloc_dma(dma_va, PAGE_SIZE);
        res.map(|_| ())
    }

    fn flush(&self) -> DeviceResult {
        let cmd = NvmeCommand {
            opcode: NVME_CMD_FLUSH,
            nsid: 1,
            ..Default::default()
        };
        self.io_q
            .lock()
            .exec_command(self.phys_nvme_base, &cmd, &mut self.rtkit.lock())
            .map(|_| ())
    }
}
