//! Minimal Apple RTKit coprocessor boot/management protocol.
//!
//! RTKit is the boot and system-message protocol Apple's SoC coprocessors
//! (ANS2 storage, SEP, ...) speak over an [`AppleMailbox`]. This implements
//! just enough of it to boot a SART-based (no DART/IOMMU) coprocessor and
//! keep servicing its system endpoints — crashlog/syslog/ioreport buffer
//! requests — so the IOP doesn't stall mid-command waiting on a management
//! reply that never comes. Ported from m1n1's `src/rtkit.c`.
//!
//! Deliberately out of scope: DART-backed coprocessors, SRAM-backed buffers,
//! and orderly shutdown/sleep (`rtkit_quiesce`/`rtkit_sleep` in m1n1) —
//! zCore never tears this driver down.

use crate::nvme::nvme_queue::{Provider, ProviderImpl, PAGE_SIZE};

use super::apple_mailbox::{AppleMailbox, MailboxMessage};
use super::apple_sart::AppleSart;
use super::hvcall;

const EP_MGMT: u8 = 0;
const EP_CRASHLOG: u8 = 1;
const EP_SYSLOG: u8 = 2;
const EP_DEBUG: u8 = 3;
const EP_IOREPORT: u8 = 4;
const EP_OSLOG: u8 = 8;

// `MGMT_TYPE`: GENMASK(59, 52), the message-kind field reused by every
// endpoint's protocol, not just EP_MGMT.
fn msg_type(msg0: u64) -> u64 {
    (msg0 >> 52) & 0xff
}
fn make_type(ty: u64) -> u64 {
    (ty & 0xff) << 52
}

// `MGMT_PWR_STATE`: GENMASK(15, 0).
fn pwr_state(msg0: u64) -> u64 {
    msg0 & 0xffff
}

const MSG_BUFFER_REQUEST: u64 = 1;
// `MSG_BUFFER_REQUEST_SIZE`: GENMASK(51, 44), count of 4K pages.
fn buffer_request_pages(msg0: u64) -> u64 {
    (msg0 >> 44) & 0xff
}
// `MSG_BUFFER_REQUEST_IOVA`: GENMASK(41, 0).
fn buffer_request_iova(msg0: u64) -> u64 {
    msg0 & ((1u64 << 42) - 1)
}

const MSG_SYSLOG_LOG: u64 = 5;

const MGMT_MSG_HELLO: u64 = 1;
const MGMT_MSG_HELLO_ACK: u64 = 2;
const MGMT_MSG_IOP_PWR_STATE: u64 = 6;
const MGMT_MSG_IOP_PWR_STATE_ACK: u64 = 7;
const MGMT_MSG_EPMAP: u64 = 8;
const MGMT_MSG_EPMAP_DONE: u64 = 1 << 51;
const MGMT_MSG_EPMAP_MORE: u64 = 1 << 0;
const MGMT_MSG_AP_PWR_STATE: u64 = 0xb;
const MGMT_MSG_AP_PWR_STATE_ACK: u64 = 0xb;
const MGMT_MSG_START_EP: u64 = 5;
const START_EP_FLAG: u64 = 1 << 1;

const RTKIT_MIN_VERSION: u64 = 11;
const RTKIT_MAX_VERSION: u64 = 12;

const POWER_ON: u64 = 0x20;
const POWER_INIT: u64 = 0x220;

const HANDSHAKE_SPIN_TIMEOUT: usize = 5_000_000;
const BOOT_SPIN_TIMEOUT: usize = 20_000_000;

#[derive(Default, Clone, Copy)]
struct Buffer {
    #[allow(unused)]
    va: usize,
    pa: usize,
}

pub struct AppleRtKit {
    mailbox: AppleMailbox,
    sart: AppleSart,
    iop_power: u64,
    ap_power: u64,
    syslog_bfr: Buffer,
    crashlog_bfr: Buffer,
    ioreport_bfr: Buffer,
}

impl AppleRtKit {
    pub fn new(mailbox: AppleMailbox, sart: AppleSart) -> Self {
        Self {
            mailbox,
            sart,
            iop_power: 0,
            ap_power: 0,
            syslog_bfr: Buffer::default(),
            crashlog_bfr: Buffer::default(),
            ioreport_bfr: Buffer::default(),
        }
    }

    fn send(&self, msg0: u64, ep: u8) -> bool {
        self.mailbox.send(MailboxMessage {
            msg0,
            msg1: ep as u32,
        })
    }

    fn start_ep(&self, ep: u8) -> bool {
        let msg0 = make_type(MGMT_MSG_START_EP) | START_EP_FLAG | ((ep as u64) << 32);
        self.send(msg0, EP_MGMT)
    }

    /// Boots the coprocessor: starts the ASC CPU (`cpu_ctrl_base + 0x44`),
    /// then performs the RTKit HELLO/endpoint-map handshake.
    ///
    /// `cpu_ctrl_base` is `/arm-io/ans` reg 0 (virtual, for the read);
    /// `phys_cpu_ctrl_base` is the same window's physical address, used for
    /// the write via [`hvcall`] (see its module docs — the SoC fabric NAKs
    /// this write from an EL1 guest under m1n1's hypervisor). m1n1
    /// `asc_cpu_start()` writes `ASC_CPU_CONTROL` (0x44)
    /// `ASC_CPU_CONTROL_START` (BIT(4)). Mailbox FIFOs at `ans+0x8000` NAK
    /// until that bit is set.
    pub fn boot(&mut self, cpu_ctrl_base: usize, phys_cpu_ctrl_base: usize) -> bool {
        const ASC_CPU_CONTROL: usize = 0x44;
        const ASC_CPU_CONTROL_START: u32 = 1 << 4;
        if cpu_ctrl_base != 0 {
            unsafe {
                let addr = (cpu_ctrl_base + ASC_CPU_CONTROL) as *const u32;
                let cur = core::ptr::read_volatile(addr);
                // Linux only writel(RUN) when the bit is clear. A write while
                // the coprocessor is already running nacks on T6020 after a
                // prior-stage boot (m1n1).
                if cur & ASC_CPU_CONTROL_START == 0 {
                    hvcall::hv_write(
                        phys_cpu_ctrl_base + ASC_CPU_CONTROL,
                        4,
                        ASC_CPU_CONTROL_START as u64,
                    );
                }
            }
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        }

        // m1n1 chainload leaves ANS running; only the CPU_CONTROL.RUN write
        // is skipped above in that case (a redundant write nacks). The
        // mailbox HELLO/EPMAP handshake below still runs unconditionally:
        // an earlier attempt to skip it (and go straight to raw NVMe
        // register writes) found those writes themselves NAK'd, suggesting
        // the coprocessor firmware expects the AP to complete the RTKit
        // wake sequence before authorizing further access — matching
        // Linux's `apple_rtkit_wake()`, which always redoes the handshake.

        // Unconditional wakeup kick: revives an IOP left sleeping from a
        // prior boot stage's use of this coprocessor.
        if !self.send(make_type(MGMT_MSG_IOP_PWR_STATE) | POWER_INIT, EP_MGMT) {
            error!("rtkit: unable to send wakeup message");
            return false;
        }

        let Some(hello) = self.mailbox.recv_timeout(HANDSHAKE_SPIN_TIMEOUT) else {
            error!("rtkit: did not receive HELLO");
            return false;
        };
        if (hello.msg1 & 0xff) as u8 != EP_MGMT {
            error!(
                "rtkit: expected HELLO but got a message for endpoint {:#x}",
                hello.msg1
            );
            return false;
        }
        if msg_type(hello.msg0) != MGMT_MSG_HELLO {
            error!(
                "rtkit: expected HELLO but got message type {:#x}",
                msg_type(hello.msg0)
            );
            return false;
        }

        let min_ver = hello.msg0 & 0xffff;
        let max_ver = (hello.msg0 >> 16) & 0xffff;
        if min_ver > RTKIT_MAX_VERSION || max_ver < RTKIT_MIN_VERSION {
            error!(
                "rtkit: no overlap between our version range [{},{}] and the IOP's [{},{}]",
                RTKIT_MIN_VERSION, RTKIT_MAX_VERSION, min_ver, max_ver
            );
            return false;
        }
        let want_ver = core::cmp::min(RTKIT_MAX_VERSION, max_ver);
        if !self.send(
            make_type(MGMT_MSG_HELLO_ACK) | want_ver | (want_ver << 16),
            EP_MGMT,
        ) {
            error!("rtkit: could not send HELLO ack");
            return false;
        }

        let (mut has_crashlog, mut has_debug, mut has_ioreport) = (false, false, false);
        let (mut has_syslog, mut has_oslog, mut got_epmap) = (false, false, false);
        while !got_epmap {
            let Some(msg) = self.mailbox.recv_timeout(HANDSHAKE_SPIN_TIMEOUT) else {
                error!("rtkit: timed out waiting for the endpoint map");
                return false;
            };
            if (msg.msg1 & 0xff) as u8 != EP_MGMT {
                error!("rtkit: expected a management message while waiting for the endpoint map");
                return false;
            }
            if msg_type(msg.msg0) != MGMT_MSG_EPMAP {
                error!(
                    "rtkit: expected an endpoint map message, got type {:#x}",
                    msg_type(msg.msg0)
                );
                return false;
            }

            let bitmap = (msg.msg0 & 0xffff_ffff) as u32;
            let base = ((msg.msg0 >> 32) & 0x7) as u32;
            for i in 0..32u8 {
                if bitmap & (1 << i) == 0 {
                    continue;
                }
                let idx = 32 * base as u16 + i as u16;
                if idx >= 0x20 {
                    continue;
                }
                match idx as u8 {
                    EP_CRASHLOG => has_crashlog = true,
                    EP_DEBUG => has_debug = true,
                    EP_IOREPORT => has_ioreport = true,
                    EP_SYSLOG => has_syslog = true,
                    EP_OSLOG => has_oslog = true,
                    EP_MGMT => {}
                    other => debug!("rtkit: unknown system endpoint {:#x}", other),
                }
            }

            got_epmap = msg.msg0 & MGMT_MSG_EPMAP_DONE != 0;
            let mut reply = make_type(MGMT_MSG_EPMAP) | ((base as u64) << 32);
            reply |= if got_epmap {
                MGMT_MSG_EPMAP_DONE
            } else {
                MGMT_MSG_EPMAP_MORE
            };
            if !self.send(reply, EP_MGMT) {
                error!("rtkit: could not reply to the endpoint map");
                return false;
            }
        }

        if (has_debug && !self.start_ep(EP_DEBUG))
            || (has_crashlog && !self.start_ep(EP_CRASHLOG))
            || (has_syslog && !self.start_ep(EP_SYSLOG))
            || (has_ioreport && !self.start_ep(EP_IOREPORT))
            || (has_oslog && !self.start_ep(EP_OSLOG))
        {
            error!("rtkit: unable to start a system endpoint");
            return false;
        }

        let mut spins = 0;
        while self.iop_power != POWER_ON {
            if let Some(m) = self.mailbox.try_recv() {
                if !self.dispatch(m) {
                    return false;
                }
            } else {
                spins += 1;
                if spins > BOOT_SPIN_TIMEOUT {
                    error!("rtkit: timed out waiting for the IOP to reach power-on");
                    return false;
                }
                core::hint::spin_loop();
            }
        }

        // Tells the IOP the AP is up too; this is what turns on syslog
        // delivery.
        if !self.send(make_type(MGMT_MSG_AP_PWR_STATE) | POWER_ON, EP_MGMT) {
            error!("rtkit: unable to send the AP power-state message");
            return false;
        }

        true
    }

    /// Drains and dispatches every pending system-endpoint message (power
    /// acks, buffer requests, syslog acks, ...). Call this periodically
    /// while waiting on a long-running device command so the IOP's mailbox
    /// doesn't back up behind an unserviced management request. Returns
    /// `false` if the IOP reported an unrecoverable condition.
    pub fn poll(&mut self) -> bool {
        while let Some(m) = self.mailbox.try_recv() {
            if !self.dispatch(m) {
                return false;
            }
        }
        true
    }

    fn dispatch(&mut self, m: MailboxMessage) -> bool {
        let ep = (m.msg1 & 0xff) as u8;
        // Endpoints >= 0x20 are the device's own application protocol
        // (NVMe admin/IO queues for ANS2); nothing for RTKit itself to do.
        if ep >= 0x20 {
            return true;
        }
        let ty = msg_type(m.msg0);
        match ep {
            EP_MGMT => match ty {
                MGMT_MSG_IOP_PWR_STATE_ACK => self.iop_power = pwr_state(m.msg0),
                MGMT_MSG_AP_PWR_STATE_ACK => self.ap_power = pwr_state(m.msg0),
                other => debug!("rtkit: unknown management message type {:#x}", other),
            },
            EP_SYSLOG => match ty {
                MSG_BUFFER_REQUEST => return self.handle_buffer_request(m, EP_SYSLOG),
                MSG_SYSLOG_LOG if !self.mailbox.send(m) => {
                    // Echo the message back unmodified to acknowledge it; we
                    // don't decode/print the log text.
                    debug!("rtkit: failed to ack a syslog message");
                }
                _ => {}
            },
            EP_CRASHLOG => {
                if ty == MSG_BUFFER_REQUEST {
                    if self.crashlog_bfr.pa != 0 {
                        error!("rtkit: coprocessor crashed (unexpected repeat crashlog buffer request)");
                        return false;
                    }
                    return self.handle_buffer_request(m, EP_CRASHLOG);
                }
            }
            EP_IOREPORT => match ty {
                MSG_BUFFER_REQUEST => return self.handle_buffer_request(m, EP_IOREPORT),
                // Unknown but must be ACKed, per m1n1.
                0x8 | 0xc if !self.mailbox.send(m) => {
                    debug!("rtkit: unable to ack an unknown ioreport message");
                }
                _ => {}
            },
            EP_OSLOG => debug!("rtkit: unhandled oslog message {:#x}", m.msg0),
            other => debug!("rtkit: message for unknown system endpoint {:#x}", other),
        }
        true
    }

    fn handle_buffer_request(&mut self, m: MailboxMessage, ep: u8) -> bool {
        let n_pages = buffer_request_pages(m.msg0);
        let iova = buffer_request_iova(m.msg0);
        if iova != 0 {
            error!(
                "rtkit: buffer request for endpoint {:#x} supplied a DVA ({:#x}) but there is no DART to translate it",
                ep, iova
            );
            return false;
        }

        let want = ((n_pages as usize) << 12).max(1);
        let alloc_size = (want + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        let (va, pa) = ProviderImpl::alloc_dma(alloc_size);
        unsafe { core::ptr::write_bytes(va as *mut u8, 0, alloc_size) };

        if !self.sart.add_allowed_region(pa, alloc_size) {
            error!(
                "rtkit: SART has no free entry for a {} KiB buffer (endpoint {:#x})",
                alloc_size / 1024,
                ep
            );
            ProviderImpl::dealloc_dma(va, alloc_size);
            return false;
        }

        let buf = Buffer { va, pa };
        match ep {
            EP_SYSLOG => self.syslog_bfr = buf,
            EP_CRASHLOG => self.crashlog_bfr = buf,
            EP_IOREPORT => self.ioreport_bfr = buf,
            _ => {}
        }

        let reply0 = make_type(MSG_BUFFER_REQUEST) | (n_pages << 44) | (pa as u64);
        self.send(reply0, ep)
    }

    /// Whitelists `[pa, pa + size)` for this coprocessor's DMA, via the
    /// same SART instance used for its own crashlog/syslog/ioreport
    /// buffers. For use by higher-level drivers (e.g. `AppleAnsNvme`) whose
    /// own DMA buffers (queues, command data) the coprocessor also needs
    /// SART clearance to touch.
    pub fn sart_add_allowed_region(&mut self, pa: usize, size: usize) -> bool {
        self.sart.add_allowed_region(pa, size)
    }

    /// Removes a previously-added SART allow-list entry. See
    /// [`Self::sart_add_allowed_region`].
    pub fn sart_remove_allowed_region(&mut self, pa: usize, size: usize) -> bool {
        self.sart.remove_allowed_region(pa, size)
    }
}
