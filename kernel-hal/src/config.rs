use crate::utils::init_once::InitOnce;

pub use super::imp::config::KernelConfig;

#[cfg(all(target_os = "none", target_arch = "aarch64"))]
pub use super::imp::config::AppleMmio;

#[cfg(feature = "libos")]
pub(crate) static KCONFIG: InitOnce<KernelConfig> = InitOnce::new_with_default(KernelConfig);

#[cfg(not(feature = "libos"))]
pub(crate) static KCONFIG: InitOnce<KernelConfig> = InitOnce::new();

pub const MAX_CORE_NUM: usize = 8;
