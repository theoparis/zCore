use super::Scheme;
use crate::DeviceResult;

pub trait BlockScheme: Scheme {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> DeviceResult;
    fn write_block(&self, block_id: usize, buf: &[u8]) -> DeviceResult;
    fn flush(&self) -> DeviceResult;
    /// Total capacity in 512-byte sectors.
    fn block_count(&self) -> usize {
        0
    }
    /// Prepare the device for a warm reset / power-off.
    fn quiesce_for_reboot(&self) {
        let _ = self.flush();
    }
}
