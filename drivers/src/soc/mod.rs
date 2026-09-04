//! Generic Apple SoC coprocessor infrastructure.
//!
//! Apple's ASC-based coprocessors (ANS2 storage, SEP, ...) all speak the
//! same RTKit boot/management protocol over the same mailbox hardware, and
//! (when they have no DART/IOMMU of their own) use the same SART DMA
//! allow-list filter. This module implements that shared machinery so that
//! device-specific drivers (e.g. [`crate::nvme::AppleAnsNvme`]) only need to
//! layer their own command protocol on top.

pub mod apple_mailbox;
pub mod apple_rtkit;
pub mod apple_sart;
pub mod hvcall;

pub use apple_mailbox::{AppleMailbox, MailboxMessage};
pub use apple_rtkit::AppleRtKit;
pub use apple_sart::AppleSart;
