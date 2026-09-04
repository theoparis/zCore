cfg_if! {
    if #[cfg(feature = "linux")] {
        use alloc::sync::Arc;
        use rcore_fs::vfs::FileSystem;

        #[cfg(feature = "libos")]
        #[cfg_attr(feature = "zircon", allow(dead_code))]
        pub fn rootfs() -> Arc<dyn FileSystem> {
            let  rootfs = if let Ok(dir) = std::env::var("CARGO_MANIFEST_DIR") {
                std::path::Path::new(&dir).parent().unwrap().to_path_buf()
            } else {
                std::env::current_dir().unwrap()
            };
            rcore_fs_hostfs::HostFS::new(rootfs.join("rootfs").join("libos"))
        }

        #[cfg(not(feature = "libos"))]
        pub fn rootfs() -> Arc<dyn FileSystem> {
            use rcore_fs::dev::Device;

            let device: Arc<dyn Device> = {
                #[cfg(feature = "mock-disk")]{
                    let block = linux_object::fs::mock_block();
                    Arc::new(block)
                }
                #[cfg(not(feature = "mock-disk"))] {
                    use linux_object::fs::rcore_fs_wrapper::*;
                    if let Some(initrd) = init_ram_disk() {
                        Arc::new(MemBuf::new(initrd))
                    } else {
                        let block = kernel_hal::drivers::all_block().first_unwrap();
                        find_rootfs_device(block)
                    }
                }
            };
            info!("Opening the rootfs...");
            rcore_fs_sfs::SimpleFileSystem::open(device).expect("failed to open device SimpleFS")
        }
    } else if #[cfg(feature = "zircon")] {

        #[cfg(feature = "libos")]
        pub fn zbi() -> impl AsRef<[u8]> {
            let path = std::env::args().nth(1).unwrap();
            std::fs::read(path).expect("failed to read zbi file")
        }

        #[cfg(not(feature = "libos"))]
        pub fn zbi() -> impl AsRef<[u8]> {
            init_ram_disk().expect("failed to get the init RAM disk")
        }
    }
}

/// Scans a block device for a zCore SimpleFS rootfs (either unpartitioned, GPT partition, or MBR partition).
#[cfg(all(feature = "linux", not(feature = "libos")))]
fn find_rootfs_device(
    block: alloc::sync::Arc<dyn kernel_hal::drivers::scheme::BlockScheme>,
) -> alloc::sync::Arc<dyn rcore_fs::dev::Device> {
    use linux_object::fs::rcore_fs_wrapper::*;

    const SFS_MAGIC_V1: u32 = 0x2f8d_be2a;
    const SFS_MAGIC_V2: u32 = 0x2f8d_be2b;
    const GPT_SIGNATURE: u64 = 0x5452_4150_2049_4645; // "EFI PART" in LE

    let is_sfs = |buf: &[u8]| -> bool {
        if buf.len() >= 4 {
            let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
            magic == SFS_MAGIC_V1 || magic == SFS_MAGIC_V2
        } else {
            false
        }
    };

    let mut sector = [0u8; 512];

    // 1. Check if raw block 0 is a SimpleFS superblock directly (e.g. unpartitioned drive / RAM disk / virtio)
    let ok0 = block.read_block(0, &mut sector).is_ok();
    error!(
        "rootfs scan: read_block(0)={} bytes[0..16]={:02x?}",
        ok0,
        &sector[0..16]
    );
    if ok0 && is_sfs(&sector) {
        info!("Found zCore SimpleFS rootfs on unpartitioned block device");
        return alloc::sync::Arc::new(BlockCache::new(Block::new(block), 0x100));
    }

    // 2. Scan GPT partition table. The GPT header lives at "native LBA 1", whose
    // byte offset depends on the device's native LBA size (512 or 4096 are both
    // common on NVMe SSDs), not always our BlockScheme's normalized 512-byte
    // `block_id` unit. Try both.
    for lba_sectors in [1usize, 8usize] {
        if !block.read_block(lba_sectors, &mut sector).is_ok() {
            continue;
        }
        error!(
            "rootfs scan: read_block({})={} bytes[0..16]={:02x?}",
            lba_sectors,
            true,
            &sector[0..16]
        );

        let mut sig_bytes = [0u8; 8];
        sig_bytes.copy_from_slice(&sector[0..8]);
        let sig = u64::from_le_bytes(sig_bytes);

        if sig != GPT_SIGNATURE {
            continue;
        }

        let mut entries_lba_bytes = [0u8; 8];
        entries_lba_bytes.copy_from_slice(&sector[72..80]);
        let entries_lba = u64::from_le_bytes(entries_lba_bytes) as usize;

        let mut num_entries_bytes = [0u8; 4];
        num_entries_bytes.copy_from_slice(&sector[80..84]);
        let num_entries = u32::from_le_bytes(num_entries_bytes) as usize;

        let mut entry_size_bytes = [0u8; 4];
        entry_size_bytes.copy_from_slice(&sector[84..88]);
        let entry_size = u32::from_le_bytes(entry_size_bytes) as usize;

        if entry_size < 128 || num_entries == 0 || entries_lba == 0 {
            continue;
        }

        // `entries_lba`, and later `start_lba`/`end_lba`, are all expressed in
        // the GPT's native LBA unit — convert to our normalized 512-byte
        // `block_id` unit using the same `lba_sectors` multiplier.
        let entries_per_sector = 512 / entry_size;
        let entries_lba_512 = entries_lba * lba_sectors;
        let bytes_per_native_lba = lba_sectors * 512;
        let entries_per_native_lba = (bytes_per_native_lba / entry_size).max(1);
        let sectors_to_read = num_entries.div_ceil(entries_per_native_lba);

        for sec_idx in 0..sectors_to_read.min(32) {
            let mut entry_sector = [0u8; 512];
            for sub in 0..lba_sectors.max(1) {
                if !block
                    .read_block(
                        entries_lba_512 + sec_idx * lba_sectors + sub,
                        &mut entry_sector,
                    )
                    .is_ok()
                {
                    continue;
                }
                for i in 0..entries_per_sector {
                    let entry_offset = i * entry_size;
                    if entry_offset + entry_size > entry_sector.len() {
                        break;
                    }
                    let entry = &entry_sector[entry_offset..entry_offset + entry_size];
                    let type_guid = &entry[0..16];
                    if type_guid == [0u8; 16] {
                        continue;
                    }

                    let mut start_lba_bytes = [0u8; 8];
                    start_lba_bytes.copy_from_slice(&entry[32..40]);
                    let start_lba = u64::from_le_bytes(start_lba_bytes) as usize * lba_sectors;

                    let mut end_lba_bytes = [0u8; 8];
                    end_lba_bytes.copy_from_slice(&entry[40..48]);
                    let end_lba = u64::from_le_bytes(end_lba_bytes) as usize * lba_sectors
                        + (lba_sectors - 1);

                    if start_lba > 0 && end_lba >= start_lba {
                        let mut probe_buf = [0u8; 512];
                        if block.read_block(start_lba, &mut probe_buf).is_ok() && is_sfs(&probe_buf)
                        {
                            let part_num =
                                (sec_idx * lba_sectors + sub) * entries_per_sector + i + 1;
                            let block_count = end_lba - start_lba + 1;
                            info!(
                                "Found zCore SimpleFS rootfs on GPT partition {} at LBA {} (size: {} sectors / {} MB)",
                                part_num,
                                start_lba,
                                block_count,
                                (block_count * 512) / (1024 * 1024)
                            );
                            return alloc::sync::Arc::new(BlockCache::new(
                                PartitionBlock::new(block, start_lba, block_count),
                                0x100,
                            ));
                        }
                    }
                }
            }
        }
    }

    // 3. Fallback: Scan MBR partitions (LBA 0, bytes 446..510)
    if block.read_block(0, &mut sector).is_ok() && sector[510] == 0x55 && sector[511] == 0xAA {
        for i in 0..4 {
            let offset = 446 + i * 16;
            let mut start_bytes = [0u8; 4];
            start_bytes.copy_from_slice(&sector[offset + 8..offset + 12]);
            let start_lba = u32::from_le_bytes(start_bytes) as usize;

            let mut count_bytes = [0u8; 4];
            count_bytes.copy_from_slice(&sector[offset + 12..offset + 16]);
            let count = u32::from_le_bytes(count_bytes) as usize;

            if start_lba > 0 && count > 0 {
                let mut probe_buf = [0u8; 512];
                if block.read_block(start_lba, &mut probe_buf).is_ok() && is_sfs(&probe_buf) {
                    info!(
                        "Found zCore SimpleFS rootfs on MBR partition {} at LBA {} (size: {} sectors)",
                        i + 1,
                        start_lba,
                        count
                    );
                    return alloc::sync::Arc::new(BlockCache::new(
                        PartitionBlock::new(block, start_lba, count),
                        0x100,
                    ));
                }
            }
        }
    }

    warn!("No partitioned SimpleFS found, falling back to raw block device");
    alloc::sync::Arc::new(BlockCache::new(Block::new(block), 0x100))
}

#[cfg(not(feature = "libos"))]
pub(crate) fn init_ram_disk() -> Option<&'static mut [u8]> {
    if cfg!(feature = "link-user-img") {
        unsafe extern "C" {
            fn _user_img_start();
            fn _user_img_end();
        }
        Some(unsafe {
            core::slice::from_raw_parts_mut(
                _user_img_start as *mut u8,
                _user_img_end as *const () as usize - _user_img_start as *const () as usize,
            )
        })
    } else {
        kernel_hal::boot::init_ram_disk()
    }
}

// Hard link rootfs img
#[cfg(not(feature = "libos"))]
#[cfg(feature = "link-user-img")]
core::arch::global_asm!(concat!(
    r#"
    .section .data.img
    .global _user_img_start
    .global _user_img_end
_user_img_start:
    .incbin ""#,
    env!("USER_IMG"),
    r#""
_user_img_end:
"#
));
