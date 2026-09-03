//! GSP (GPU System Processor) RPC and Queue Interfaces
//!
//! Turing (TU10x) and newer NVIDIA GPUs incorporate a RISC-V/Falcon GSP core
//! executing on-chip resource management firmware.
//! The host driver interacts with the GSP via command/status queues (DMA ring buffers)
//! and RPC message packets.

use core::sync::atomic::{AtomicU32, Ordering};

/// GSP Message Header
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, Default)]
pub struct GspMessageHeader {
    pub magic: u32,
    pub length: u32,
    pub sequence: u32,
    pub result: u32,
    pub function: u32,
    pub flags: u32,
}

pub const GSP_MSG_MAGIC: u32 = 0x4753504D; // 'GSPM'

/// Common GSP RPC function codes (aligned with open-gpu-kernel-modules / GSP-RM protocol)
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GspRpcFunction {
    Nop = 0x00,
    GetCapabilities = 0x01,
    AllocClient = 0x02,
    FreeClient = 0x03,
    AllocDevice = 0x04,
    FreeDevice = 0x05,
    AllocSubdevice = 0x06,
    FreeSubdevice = 0x07,
    AllocMemory = 0x08,
    FreeMemory = 0x09,
    AllocChannel = 0x0A,
    FreeChannel = 0x0B,
    SetDisplayMode = 0x10,
    GetDisplayInfo = 0x11,
}

/// GSP Ring Buffer Queue Descriptor
pub struct GspQueue {
    pub base_paddr: u64,
    pub base_vaddr: usize,
    pub size: usize,
    pub head: AtomicU32,
    pub tail: AtomicU32,
}

impl GspQueue {
    pub fn new(base_paddr: u64, base_vaddr: usize, size: usize) -> Self {
        Self {
            base_paddr,
            base_vaddr,
            size,
            head: AtomicU32::new(0),
            tail: AtomicU32::new(0),
        }
    }

    pub fn head(&self) -> u32 {
        self.head.load(Ordering::Acquire)
    }

    pub fn tail(&self) -> u32 {
        self.tail.load(Ordering::Acquire)
    }

    pub fn set_head(&self, head: u32) {
        self.head.store(head, Ordering::Release);
    }

    pub fn set_tail(&self, tail: u32) {
        self.tail.store(tail, Ordering::Release);
    }
}
