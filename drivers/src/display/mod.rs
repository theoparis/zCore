//! Only UEFI Display currently.

mod uefi;

pub use uefi::UefiDisplay;

static NOUVEAU_UAPI_ENABLED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Whether the nouveau-compatible uAPI is currently enabled.
pub fn nouveau_uapi_enabled() -> bool {
    NOUVEAU_UAPI_ENABLED.load(core::sync::atomic::Ordering::Relaxed)
}

/// Set whether the nouveau-compatible uAPI is enabled.
pub fn set_nouveau_uapi_enabled(v: bool) {
    NOUVEAU_UAPI_ENABLED.store(v, core::sync::atomic::Ordering::Relaxed);
}

/// Set RM thread ID provider.
pub fn set_rm_thread_id_provider(_f: fn() -> u64) {}

/// Set boot FB info.
pub fn set_boot_fb_info(_addr: u64, _w: u32, _h: u32, _pitch: u32) {}

/// Set boot EDID.
pub fn set_boot_edid(_edid: &[u8], _size: usize) {}

/// Get boot EDID.
pub fn boot_edid() -> Option<([u8; 128], u32)> {
    None
}
