//! Device drivers.

use alloc::{sync::Arc, vec::Vec};
use core::convert::From;

use crate::sync::{RwLock, RwLockReadGuard};

use zcore_drivers::scheme::{
    BlockScheme, DisplayScheme, DrmScheme, InputScheme, IrqScheme, NetScheme, Scheme, UartScheme,
};
use zcore_drivers::{Device, DeviceError};

/// Re-exported modules from crate [`zcore_drivers`].
pub use zcore_drivers::{prelude, scheme};

/// A wrapper of a device array with the same [`Scheme`].
pub struct DeviceList<T: Scheme + ?Sized>(RwLock<Vec<Arc<T>>>);

impl<T: Scheme + ?Sized> DeviceList<T> {
    fn add(&self, dev: Arc<T>) {
        self.0.write().push(dev);
    }

    /// Convert self into a vector.
    pub fn as_vec(&self) -> RwLockReadGuard<'_, Vec<Arc<T>>> {
        self.0.read()
    }

    /// Returns the device at given position, or `None` if out of bounds.
    pub fn try_get(&self, idx: usize) -> Option<Arc<T>> {
        self.0.read().get(idx).cloned()
    }

    /// Returns the device with the given name, or `None` if not found.
    pub fn find(&self, name: &str) -> Option<Arc<T>> {
        self.0.read().iter().find(|d| d.name() == name).cloned()
    }

    /// Returns the first device of this device array, or `None` if it is empty.
    pub fn first(&self) -> Option<Arc<T>> {
        self.try_get(0)
    }

    /// Returns the first device of this device array.
    ///
    /// # Panic
    ///
    /// Panics if the array is empty.
    pub fn first_unwrap(&self) -> Arc<T> {
        self.first()
            .unwrap_or_else(|| panic!("device not initialized: {}", core::any::type_name::<T>()))
    }
}

impl<T: Scheme + ?Sized> Default for DeviceList<T> {
    fn default() -> Self {
        Self(RwLock::new(Vec::new()))
    }
}

#[derive(Default)]
struct AllDeviceList {
    block: DeviceList<dyn BlockScheme>,
    display: DeviceList<dyn DisplayScheme>,
    input: DeviceList<dyn InputScheme>,
    irq: DeviceList<dyn IrqScheme>,
    net: DeviceList<dyn NetScheme>,
    uart: DeviceList<dyn UartScheme>,
    drm: DeviceList<dyn DrmScheme>,
}

impl AllDeviceList {
    pub fn add_device(&self, dev: Device) {
        match dev {
            Device::Block(d) => self.block.add(d),
            Device::Display(d) => self.display.add(d),
            Device::Input(d) => self.input.add(d),
            Device::Irq(d) => self.irq.add(d),
            Device::Net(d) => self.net.add(d),
            Device::Uart(d) => self.uart.add(d),
            Device::Drm(d) => self.drm.add(d),
        }
    }
}

lazy_static! {
    static ref DEVICES: AllDeviceList = AllDeviceList::default();
}

pub(crate) fn add_device(dev: Device) {
    DEVICES.add_device(dev)
}

/// Returns all devices which implement the [`BlockScheme`].
pub fn all_block() -> &'static DeviceList<dyn BlockScheme> {
    &DEVICES.block
}

/// Returns all devices which implement the [`DisplayScheme`].
pub fn all_display() -> &'static DeviceList<dyn DisplayScheme> {
    &DEVICES.display
}

/// Returns all devices which implement the [`InputScheme`].
pub fn all_input() -> &'static DeviceList<dyn InputScheme> {
    &DEVICES.input
}

/// Returns all devices which implement the [`IrqScheme`].
pub fn all_irq() -> &'static DeviceList<dyn IrqScheme> {
    &DEVICES.irq
}

/// Returns all devices which implement the [`NetScheme`].
pub fn all_net() -> &'static DeviceList<dyn NetScheme> {
    &DEVICES.net
}

/// Returns all devices which implement the [`UartScheme`].
pub fn all_uart() -> &'static DeviceList<dyn UartScheme> {
    &DEVICES.uart
}

/// Returns all devices which implement the [`DrmScheme`].
pub fn all_drm() -> &'static DeviceList<dyn DrmScheme> {
    &DEVICES.drm
}

/// Enables the nouveau-compatible driver-specific ioctl surface on the
/// NVIDIA DRM driver.
#[cfg(target_arch = "x86_64")]
pub fn set_nouveau_uapi_enabled(v: bool) {
    zcore_drivers::display::set_nouveau_uapi_enabled(v);
}
#[cfg(not(target_arch = "x86_64"))]
pub fn set_nouveau_uapi_enabled(_v: bool) {}

/// Hands the NVIDIA RM a provider of real per-thread identity.
#[cfg(target_arch = "x86_64")]
pub fn set_rm_thread_id_provider(f: fn() -> u64) {
    zcore_drivers::display::set_rm_thread_id_provider(f);
}
#[cfg(not(target_arch = "x86_64"))]
pub fn set_rm_thread_id_provider(_f: fn() -> u64) {}

/// Whether the nouveau-compatible ioctl surface is currently enabled.
#[cfg(target_arch = "x86_64")]
pub fn nouveau_uapi_enabled() -> bool {
    zcore_drivers::display::nouveau_uapi_enabled()
}
#[cfg(not(target_arch = "x86_64"))]
pub fn nouveau_uapi_enabled() -> bool {
    false
}

impl From<DeviceError> for crate::HalError {
    fn from(err: DeviceError) -> Self {
        warn!("{:?}", err);
        Self
    }
}

#[cfg(not(feature = "libos"))]
mod virtio_drivers_ffi {
    use crate::{PhysAddr, VirtAddr, KCONFIG, KHANDLER, PAGE_SIZE};

    #[unsafe(no_mangle)]
    extern "C" fn virtio_dma_alloc(pages: usize) -> PhysAddr {
        let paddr = KHANDLER.frame_alloc_contiguous(pages, 0).unwrap();
        trace!("alloc DMA: paddr={:#x}, pages={}", paddr, pages);
        paddr
    }

    #[unsafe(no_mangle)]
    extern "C" fn virtio_dma_dealloc(paddr: PhysAddr, pages: usize) -> i32 {
        for i in 0..pages {
            KHANDLER.frame_dealloc(paddr + i * PAGE_SIZE);
        }
        trace!("dealloc DMA: paddr={:#x}, pages={}", paddr, pages);
        0
    }

    #[unsafe(no_mangle)]
    extern "C" fn virtio_phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        paddr + KCONFIG.phys_to_virt_offset
    }

    #[unsafe(no_mangle)]
    extern "C" fn virtio_virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
        vaddr - KCONFIG.phys_to_virt_offset
    }
}

#[cfg(not(feature = "libos"))]
mod drivers_ffi {
    use crate::{PhysAddr, VirtAddr, KCONFIG, KHANDLER, PAGE_SIZE};

    #[unsafe(no_mangle)]
    extern "C" fn drivers_dma_alloc(pages: usize) -> PhysAddr {
        let paddr = KHANDLER.frame_alloc_contiguous(pages, 0).unwrap();
        trace!("alloc DMA: paddr={:#x}, pages={}", paddr, pages);
        paddr
    }

    #[unsafe(no_mangle)]
    extern "C" fn drivers_dma_dealloc(paddr: PhysAddr, pages: usize) -> i32 {
        for i in 0..pages {
            KHANDLER.frame_dealloc(paddr + i * PAGE_SIZE);
        }
        trace!("dealloc DMA: paddr={:#x}, pages={}", paddr, pages);
        0
    }

    #[unsafe(no_mangle)]
    extern "C" fn drivers_phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        paddr + KCONFIG.phys_to_virt_offset
    }

    #[unsafe(no_mangle)]
    extern "C" fn drivers_virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
        vaddr - KCONFIG.phys_to_virt_offset
    }

    #[unsafe(no_mangle)]
    extern "C" fn drivers_dma_mark_uncached(paddr: PhysAddr, pages: usize) -> i32 {
        use crate::hal_fn::vm::flush_tlb;
        use crate::vm::{GenericPageTable, PageTable};
        use crate::{CachePolicy, MMUFlags, PAGE_SIZE};

        if paddr == 0 || pages == 0 {
            return -1;
        }
        let vaddr = paddr + KCONFIG.phys_to_virt_offset;
        let flags = MMUFlags::READ
            | MMUFlags::WRITE
            | MMUFlags::DEVICE
            | MMUFlags::from_bits_truncate(CachePolicy::UncachedDevice as usize);
        let mut pt = PageTable::from_current();
        for i in 0..pages {
            let va = vaddr + i * PAGE_SIZE;
            match pt.query(va) {
                Ok((_, _, size)) => {
                    if size as usize != PAGE_SIZE {
                        return -1;
                    }
                    if let Err(_) = pt.update(va, None, Some(flags)) {
                        return -1;
                    }
                }
                Err(_) => {
                    if let Err(_) = pt.map_cont(va, PAGE_SIZE, paddr + i * PAGE_SIZE, flags) {
                        return -1;
                    }
                }
            }
        }
        flush_tlb(None);
        core::mem::forget(pt);
        0
    }

    #[unsafe(no_mangle)]
    extern "C" fn drivers_dma_verify_uncached(paddr: PhysAddr, pages: usize) -> i32 {
        use crate::vm::{GenericPageTable, PageTable};
        use crate::{CachePolicy, PAGE_SIZE};

        if paddr == 0 || pages == 0 {
            return -1;
        }
        let vaddr = paddr + KCONFIG.phys_to_virt_offset;
        let pt = PageTable::from_current();
        for i in 0..pages {
            let va = vaddr + i * PAGE_SIZE;
            let Ok((_, flags, _)) = pt.query(va) else {
                return -1;
            };
            let policy = flags.bits() & 3;
            if policy != CachePolicy::Uncached as usize
                && policy != CachePolicy::UncachedDevice as usize
            {
                return -1;
            }
        }
        core::mem::forget(pt);
        0
    }

    use crate::hal_fn::timer::timer_now;
    #[unsafe(no_mangle)]
    extern "C" fn drivers_timer_now_as_micros() -> u64 {
        timer_now().as_micros() as _
    }

    #[unsafe(no_mangle)]
    extern "C" fn drivers_klog_emit(_priority: u8, _msg: *const u8, _len: usize) {}
}
