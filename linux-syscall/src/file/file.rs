//! File operations
//!
//! - read, pread, readv
//! - write, pwrite, writev
//! - lseek
//! - truncate, ftruncate
//! - sendfile, copy_file_range
//! - sync, fsync, fdatasync
//! - ioctl, fcntl
//! - access, faccessat

use super::*;
use linux_object::{process::FsInfo, time::TimeSpec};

impl Syscall<'_> {
    /// Reads from a specified file using a file descriptor. Before using this call,
    /// you must first obtain a file descriptor using the opensyscall. Returns bytes read successfully.
    /// - fd – file descriptor
    /// - base – pointer to the buffer to fill with read contents
    /// - len – number of bytes to read
    pub async fn sys_read(&self, fd: FileDesc, mut base: UserOutPtr<u8>, len: usize) -> SysResult {
        info!("read: fd={:?}, base={:?}, len={:#x}", fd, base, len);
        let proc = self.linux_process();
        let file_like = proc.get_file_like(fd)?;
        let mut buf = vec![0u8; len];
        let len = file_like.read(&mut buf).await?;
        base.write_array(&buf[..len])?;
        Ok(len)
    }

    /// Writes to a specified file using a file descriptor. Before using this call,
    /// you must first obtain a file descriptor using the open syscall. Returns bytes written successfully.
    /// - fd – file descriptor
    /// - base – pointer to the buffer write
    /// - len – number of bytes to write
    pub fn sys_write(&self, fd: FileDesc, base: UserInPtr<u8>, len: usize) -> SysResult {
        info!("write: fd={:?}, base={:?}, len={:#x}", fd, base, len);
        self.linux_process()
            .get_file_like(fd)?
            .write(base.as_slice(len)?)
    }

    /// read from or write to a file descriptor at a given offset
    /// reads up to count bytes from file descriptor fd at offset offset
    /// (from the start of the file) into the buffer starting at buf. The file offset is not changed.
    pub async fn sys_pread(
        &self,
        fd: FileDesc,
        mut base: UserOutPtr<u8>,
        len: usize,
        offset: u64,
    ) -> SysResult {
        info!(
            "pread: fd={:?}, base={:?}, len={}, offset={}",
            fd, base, len, offset
        );
        let proc = self.linux_process();
        let file_like = proc.get_file_like(fd)?;
        let mut buf = vec![0u8; len];
        let len = file_like.read_at(offset, &mut buf).await?;
        base.write_array(&buf[..len])?;
        Ok(len)
    }

    /// writes up to count bytes from the buffer
    /// starting at buf to the file descriptor fd at offset offset. The file offset is not changed.
    pub fn sys_pwrite(
        &self,
        fd: FileDesc,
        base: UserInPtr<u8>,
        len: usize,
        offset: u64,
    ) -> SysResult {
        info!(
            "pwrite: fd={:?}, base={:?}, len={}, offset={}",
            fd, base, len, offset
        );
        self.linux_process()
            .get_file_like(fd)?
            .write_at(offset, base.as_slice(len)?)
    }

    /// works just like read except that multiple buffers are filled.
    /// reads iov_count buffers from the file
    /// associated with the file descriptor fd into the buffers described by iov ("scatter input")
    pub async fn sys_readv(
        &self,
        fd: FileDesc,
        iov_ptr: UserInPtr<IoVecOut>,
        iov_count: usize,
    ) -> SysResult {
        info!("readv: fd={:?}, iov={:?}, count={}", fd, iov_ptr, iov_count);
        let mut iovs = iov_ptr.read_iovecs(iov_count)?;
        let proc = self.linux_process();
        let file_like = proc.get_file_like(fd)?;
        let mut buf = vec![0u8; iovs.total_len()];
        let len = file_like.read(&mut buf).await?;
        iovs.write_from_buf(&buf)?;
        Ok(len)
    }

    /// works just like write except that multiple buffers are written out.
    /// writes iov_count buffers of data described
    /// by iov to the file associated with the file descriptor fd ("gather output").
    pub fn sys_writev(
        &self,
        fd: FileDesc,
        iov_ptr: UserInPtr<IoVecIn>,
        iov_count: usize,
    ) -> SysResult {
        info!(
            "writev: fd={:?}, iov={:?}, count={}",
            fd, iov_ptr, iov_count
        );
        let iovs = iov_ptr.read_iovecs(iov_count)?;
        let buf = iovs.read_to_vec()?;
        let proc = self.linux_process();
        let file_like = proc.get_file_like(fd)?;
        let len = file_like.write(&buf)?;
        Ok(len)
    }

    /// repositions the offset of the open file associated with the file descriptor fd
    /// to the argument offset according to the directive whence
    pub fn sys_lseek(&self, fd: FileDesc, offset: i64, whence: u8) -> SysResult {
        const SEEK_SET: u8 = 0;
        const SEEK_CUR: u8 = 1;
        const SEEK_END: u8 = 2;

        let pos = match whence {
            SEEK_SET => SeekFrom::Start(offset as u64),
            SEEK_END => SeekFrom::End(offset),
            SEEK_CUR => SeekFrom::Current(offset),
            _ => return Err(LxError::EINVAL),
        };
        info!("lseek: fd={:?}, pos={:?}", fd, pos);

        let proc = self.linux_process();
        let file = proc.get_file(fd)?;
        let offset = file.seek(pos)?;
        Ok(offset as usize)
    }

    /// cause the regular file named by path to be truncated to a size of precisely length bytes.
    pub fn sys_truncate(&self, path: UserInPtr<u8>, len: usize) -> SysResult {
        let path = path.as_c_str()?;
        info!("truncate: path={:?}, len={}", path, len);
        self.linux_process().lookup_inode(path)?.resize(len)?;
        Ok(0)
    }

    /// cause the regular file referenced by fd to be truncated to a size of precisely length bytes.
    pub fn sys_ftruncate(&self, fd: FileDesc, len: usize) -> SysResult {
        info!("ftruncate: fd={:?}, len={}", fd, len);
        let proc = self.linux_process();
        proc.get_file(fd)?.set_len(len as u64)?;
        Ok(0)
    }

    /// copies data between one file descriptor and another.
    pub async fn sys_sendfile(
        &self,
        out_fd: FileDesc,
        in_fd: FileDesc,
        offset_ptr: UserInOutPtr<u64>,
        count: usize,
    ) -> SysResult {
        self.sys_copy_file_range(in_fd, offset_ptr, out_fd, 0.into(), count, 0)
            .await
    }

    /// copies data between one file descriptor and anothe, read from specified offset and write new offset back
    pub async fn sys_copy_file_range(
        &self,
        in_fd: FileDesc,
        mut in_offset: UserInOutPtr<u64>,
        out_fd: FileDesc,
        mut out_offset: UserInOutPtr<u64>,
        count: usize,
        flags: usize,
    ) -> SysResult {
        info!(
            "copy_file_range: in={:?}, out={:?}, in_offset={:?}, out_offset={:?}, count={}, flags={}",
            in_fd, out_fd, in_offset, out_offset, count, flags
        );
        let proc = self.linux_process();
        let in_file = proc.get_file(in_fd)?;
        let out_file = proc.get_file(out_fd)?;
        let mut buffer = [0u8; 1024];

        // for in_offset and out_offset
        // null means update file offset
        // non-null means update {in,out}_offset instead

        let mut read_offset = if !in_offset.is_null() {
            in_offset.read()?
        } else {
            in_file.seek(SeekFrom::Current(0))?
        };

        let orig_out_file_offset = out_file.seek(SeekFrom::Current(0))?;
        let write_offset = if !out_offset.is_null() {
            let offset = out_offset.read()?;
            out_file.seek(SeekFrom::Start(offset))?
        } else {
            0
        };

        // read from specified offset and write new offset back
        let mut bytes_read = 0;
        let mut total_written = 0;
        while bytes_read < count {
            let len = buffer.len().min(count - bytes_read);
            let read_len = in_file.read_at(read_offset, &mut buffer[..len]).await?;
            if read_len == 0 {
                break;
            }
            bytes_read += read_len;
            read_offset += read_len as u64;

            let mut bytes_written = 0;
            let mut rlen = read_len;
            while bytes_written < read_len {
                let write_len = out_file.write(&buffer[bytes_written..(bytes_written + rlen)])?;
                if write_len == 0 {
                    info!(
                        "copy_file_range:END_ERR in={:?}, out={:?}, in_offset={:?}, out_offset={:?}, count={} = bytes_read {}, bytes_written {}, write_len {}",
                        in_fd,
                        out_fd,
                        in_offset,
                        out_offset,
                        count,
                        bytes_read,
                        bytes_written,
                        write_len
                    );
                    return Err(LxError::EBADF);
                }
                bytes_written += write_len;
                rlen -= write_len;
            }
            total_written += bytes_written;
        }

        if !in_offset.is_null() {
            in_offset.write(read_offset)?;
        } else {
            in_file.seek(SeekFrom::Current(bytes_read as i64))?;
        }
        out_offset.write_if_not_null(write_offset + total_written as u64)?;
        if !out_offset.is_null() {
            out_file.seek(SeekFrom::Start(orig_out_file_offset))?;
        }
        Ok(total_written)
    }

    /// causes all buffered modifications to file metadata and data to be written to the underlying file systems.
    pub fn sys_sync(&self) -> SysResult {
        info!("sync:");
        let proc = self.linux_process();
        proc.root_inode().fs().sync()?;
        Ok(0)
    }

    /// transfers ("flushes") all modified in-core data of (i.e., modified buffer cache pages for) the file
    /// referred to by the file descriptor fd to the disk device
    pub fn sys_fsync(&self, fd: FileDesc) -> SysResult {
        info!("fsync: fd={:?}", fd);
        let proc = self.linux_process();
        proc.get_file(fd)?.sync_all()?;
        Ok(0)
    }

    /// is similar to fsync(), but does not flush modified metadata unless that metadata is needed
    pub fn sys_fdatasync(&self, fd: FileDesc) -> SysResult {
        info!("fdatasync: fd={:?}", fd);
        let proc = self.linux_process();
        proc.get_file(fd)?.sync_data()?;
        Ok(0)
    }

    /// Set parameters of device files.
    pub fn sys_ioctl(
        &self,
        fd: FileDesc,
        request: usize,
        arg1: usize,
        arg2: usize,
        arg3: usize,
    ) -> SysResult {
        info!(
            "ioctl: fd={:?}, request={:#x}, args=[{:#x}, {:#x}, {:#x}]",
            fd, request, arg1, arg2, arg3
        );
        let proc = self.linux_process();
        let file_like = proc.get_file_like(fd)?;
        let cmd = request & 0xFFFF_FFFF;
        if (cmd >> 8) & 0xFF == 0x64 {
            if let Some(ret) = self.sys_drm_prime(&file_like, cmd, arg1)? {
                return Ok(ret);
            }
            if let Some(ret) = self.sys_drm_syncobj_fd(cmd, arg1)? {
                return Ok(ret);
            }
        }
        file_like.ioctl(request, arg1, arg2, arg3)
    }

    /// DRM ioctls that must install / look up a process fd (PRIME dma-buf
    /// export-import, and CREATE_LEASE) — the inode-level DRM `io_control`
    /// cannot touch the fd table. Returns `Ok(Some(0))` if handled, `Ok(None)`
    /// if `fd` is not a DRM device (fall through to the normal ioctl path).
    fn sys_drm_prime(
        &self,
        file_like: &alloc::sync::Arc<dyn linux_object::fs::FileLike>,
        request: usize,
        arg1: usize,
    ) -> Result<Option<usize>, LxError> {
        use linux_object::fs::devfs::drm;
        use linux_object::fs::DmaBuf;

        const PRIME_HANDLE_TO_FD: usize = 0xC00C_642E; // DRM_IOWR(0x2e, drm_prime_handle)
        const PRIME_FD_TO_HANDLE: usize = 0xC00C_642D; // DRM_IOWR(0x2d, drm_prime_handle)
        const MODE_CREATE_LEASE: usize = 0xC018_64C6;

        #[repr(C)]
        #[derive(Clone, Copy, Default)]
        struct DrmPrimeHandle {
            handle: u32,
            flags: u32,
            fd: i32,
        }

        #[repr(C)]
        #[derive(Clone, Copy, Default)]
        struct DrmModeCreateLease {
            object_ids: u64,
            object_count: u32,
            flags: u32,
            lessee_id: u32,
            fd: i32,
        }

        let proc = self.linux_process();
        match request {
            PRIME_HANDLE_TO_FD | PRIME_FD_TO_HANDLE => {
                let mut ptr = UserInOutPtr::<DrmPrimeHandle>::from(arg1);
                let mut h = match ptr.read() {
                    Ok(h) => h,
                    Err(e) => {
                        warn!("[drm] PRIME read(args @ {:#x}) EFAULT: {:?}", arg1, e);
                        return Err(e.into());
                    }
                };
                let is_export = h.fd < 0;
                if is_export {
                    let (phys, size, vmo) = match drm::export_handle(h.handle) {
                        Some(v) => v,
                        None => {
                            warn!(
                                "[drm] PRIME export EINVAL: handle={} not in GEM table",
                                h.handle
                            );
                            return Err(LxError::EINVAL);
                        }
                    };
                    let dmabuf = DmaBuf::new(phys, size, vmo);
                    let new_fd = match proc.add_file(dmabuf) {
                        Ok(fd) => fd,
                        Err(e) => {
                            warn!("[drm] PRIME export add_file {:?}", e);
                            return Err(e);
                        }
                    };
                    h.fd = i32::from(new_fd);
                    log::debug!(
                        "[drm] PRIME export handle={:#x} -> phys={:#x} size={} fd={}",
                        h.handle,
                        phys,
                        size,
                        h.fd
                    );
                    if let Err(e) = ptr.write(h) {
                        warn!("[drm] PRIME export write-back EFAULT: {:?}", e);
                        return Err(e.into());
                    }
                    Ok(Some(0))
                } else {
                    let target = match proc.get_file_like(FileDesc::from(h.fd as usize)) {
                        Ok(t) => t,
                        Err(e) => {
                            warn!("[drm] PRIME import EBADF: fd={} not in fd table", h.fd);
                            return Err(e);
                        }
                    };
                    let dmabuf = target.downcast_ref::<DmaBuf>().ok_or(LxError::EINVAL)?;
                    let (handle_id, kind) = match drm::nouveau_handle_for_phys(dmabuf.phys_addr) {
                        Some(nouveau_handle) => {
                            let n = drm::nouveau_gem_add_ref(nouveau_handle);
                            log::debug!(
                                "[drm] PRIME self-import ref++ handle={:#x} -> refcount={:?}",
                                nouveau_handle,
                                n
                            );
                            (nouveau_handle, "self-import(nouveau)")
                        }
                        None => (
                            drm::import_dmabuf(dmabuf.phys_addr, dmabuf.size, dmabuf.vmo()),
                            "generic",
                        ),
                    };
                    if kind == "generic" {
                        log::error!(
                            "[drm] PRIME import fd={} phys={:#x} size={} -> generic handle={:#x} (SELF-IMPORT MISS -> GEM_INFO will ENOENT)",
                            h.fd, dmabuf.phys_addr, dmabuf.size, handle_id
                        );
                    } else {
                        log::debug!(
                            "[drm] PRIME import fd={} phys={:#x} size={} -> {} handle={:#x}",
                            h.fd,
                            dmabuf.phys_addr,
                            dmabuf.size,
                            kind,
                            handle_id
                        );
                    }
                    h.handle = handle_id;
                    ptr.write(h)?;
                    Ok(Some(0))
                }
            }
            MODE_CREATE_LEASE => {
                let mut ptr = UserInOutPtr::<DrmModeCreateLease>::from(arg1);
                let mut l = ptr.read()?;
                let lease = file_like.dup();
                let new_fd = proc.add_file(lease)?;
                l.lessee_id = 1;
                l.fd = i32::from(new_fd);
                ptr.write(l)?;
                Ok(Some(0))
            }
            _ => Ok(None),
        }
    }

    /// `SYNCOBJ_HANDLE_TO_FD` / `SYNCOBJ_FD_TO_HANDLE`
    fn sys_drm_syncobj_fd(&self, request: usize, arg1: usize) -> Result<Option<usize>, LxError> {
        use linux_object::fs::SyncobjHandle;

        const SYNCOBJ_HANDLE_TO_FD: usize = 0xC010_64C1; // DRM_IOWR(0xc1, drm_syncobj_handle)
        const SYNCOBJ_FD_TO_HANDLE: usize = 0xC010_64C2; // DRM_IOWR(0xc2, drm_syncobj_handle)
        const SYNC_FILE: u32 = 1 << 0;

        if request != SYNCOBJ_HANDLE_TO_FD && request != SYNCOBJ_FD_TO_HANDLE {
            return Ok(None);
        }
        if !kernel_hal::drivers::nouveau_uapi_enabled() {
            return Ok(None);
        }

        #[repr(C)]
        #[derive(Clone, Copy, Default)]
        struct DrmSyncobjHandle {
            handle: u32,
            flags: u32,
            fd: i32,
            pad: u32,
        }

        let proc = self.linux_process();
        let mut ptr = UserInOutPtr::<DrmSyncobjHandle>::from(arg1);
        let mut h = match ptr.read() {
            Ok(h) => h,
            Err(e) => {
                warn!("[drm] SYNCOBJ read(args @ {:#x}) EFAULT: {:?}", arg1, e);
                return Err(e.into());
            }
        };

        let sync_file = h.flags & SYNC_FILE != 0;
        if request == SYNCOBJ_HANDLE_TO_FD && sync_file {
            let Some(point) = kernel_hal::drivers::scheme::syncobj::export_snapshot(h.handle)
            else {
                warn!(
                    "[drm] SYNCOBJ_HANDLE_TO_FD(EXPORT_SYNC_FILE) EINVAL: handle={} not a live syncobj",
                    h.handle
                );
                return Err(LxError::EINVAL);
            };
            let new_fd = proc.add_file(SyncobjHandle::new_sync_file(h.handle, point))?;
            h.fd = i32::from(new_fd);
            if let Err(e) = ptr.write(h) {
                warn!("[drm] SYNCOBJ export sync_file write-back EFAULT: {:?}", e);
                return Err(e.into());
            }
            return Ok(Some(0));
        }
        if request == SYNCOBJ_FD_TO_HANDLE && sync_file {
            let target = proc.get_file_like(FileDesc::from(h.fd as usize))?;
            let file = target
                .downcast_ref::<SyncobjHandle>()
                .ok_or(LxError::EINVAL)?;
            let point = match file.sync_file_point {
                Some(p) => p,
                None => kernel_hal::drivers::scheme::syncobj::export_snapshot(file.handle)
                    .ok_or(LxError::EINVAL)?,
            };
            if !kernel_hal::drivers::scheme::syncobj::import_snapshot(h.handle, file.handle, point)
            {
                warn!(
                    "[drm] SYNCOBJ_FD_TO_HANDLE(IMPORT_SYNC_FILE) EINVAL: dst handle={} or src handle={} not live",
                    h.handle, file.handle
                );
                return Err(LxError::EINVAL);
            }
            return Ok(Some(0));
        }
        if request == SYNCOBJ_HANDLE_TO_FD {
            if kernel_hal::drivers::scheme::syncobj::query(h.handle).is_none() {
                warn!(
                    "[drm] SYNCOBJ_HANDLE_TO_FD EINVAL: handle={} not a live syncobj",
                    h.handle
                );
                return Err(LxError::EINVAL);
            }
            let file = SyncobjHandle::new(h.handle);
            let new_fd = match proc.add_file(file) {
                Ok(fd) => fd,
                Err(e) => {
                    warn!("[drm] SYNCOBJ_HANDLE_TO_FD add_file {:?}", e);
                    return Err(e);
                }
            };
            h.fd = i32::from(new_fd);
            if let Err(e) = ptr.write(h) {
                warn!("[drm] SYNCOBJ_HANDLE_TO_FD write-back EFAULT: {:?}", e);
                return Err(e.into());
            }
            Ok(Some(0))
        } else {
            let target = match proc.get_file_like(FileDesc::from(h.fd as usize)) {
                Ok(t) => t,
                Err(e) => {
                    warn!(
                        "[drm] SYNCOBJ_FD_TO_HANDLE EBADF: fd={} not in fd table",
                        h.fd
                    );
                    return Err(e);
                }
            };
            let syncobj = target
                .downcast_ref::<SyncobjHandle>()
                .ok_or(LxError::EINVAL)?;
            if syncobj.sync_file_point.is_some() {
                warn!(
                    "[drm] SYNCOBJ_FD_TO_HANDLE EINVAL: fd={} is a sync_file, not a syncobj \
                     (missing IMPORT_SYNC_FILE flag)",
                    h.fd
                );
                return Err(LxError::EINVAL);
            }
            h.handle = syncobj.handle;
            ptr.write(h)?;
            Ok(Some(0))
        }
    }

    /// Manipulate a file descriptor.
    /// - cmd – cmd flag
    /// - arg – additional parameters based on cmd
    pub fn sys_fcntl(&self, fd: FileDesc, cmd: usize, arg: usize) -> SysResult {
        info!("fcntl: fd={:?}, cmd={}, arg={}", fd, cmd, arg);
        let proc = self.linux_process();
        let file_like = proc.get_file_like(fd)?;
        if let Ok(cmd) = FcntlCmd::try_from(cmd) {
            match cmd {
                FcntlCmd::GETFD => Ok(file_like.flags().close_on_exec() as usize),
                FcntlCmd::SETFD => {
                    let mut flags = file_like.flags();
                    if (arg & 1) != 0 {
                        flags |= OpenFlags::CLOEXEC;
                    } else {
                        flags -= OpenFlags::CLOEXEC;
                    }
                    file_like.set_flags(flags)?;
                    Ok(0)
                }
                FcntlCmd::GETFL => Ok(file_like.flags().bits()),
                FcntlCmd::SETFL => {
                    file_like.set_flags(OpenFlags::from_bits_truncate(arg))?;
                    Ok(0)
                }
                FcntlCmd::DUPFD | FcntlCmd::DUPFD_CLOEXEC => {
                    let new_fd = proc.get_free_fd_from(arg);
                    self.sys_dup2(fd, new_fd)?;
                    let dup = proc.get_file_like(new_fd)?;
                    let mut flags = dup.flags();
                    if cmd == FcntlCmd::DUPFD_CLOEXEC {
                        flags |= OpenFlags::CLOEXEC;
                    } else {
                        flags -= OpenFlags::CLOEXEC;
                    }
                    dup.set_flags(flags)?;
                    Ok(new_fd.into())
                }
                _ => Err(LxError::EINVAL),
            }
        } else {
            Err(LxError::EINVAL)
        }
    }

    /// Checks whether the calling process can access the file pathname
    pub fn sys_access(&self, path: UserInPtr<u8>, mode: usize) -> SysResult {
        self.sys_faccessat(FileDesc::CWD, path, mode, 0)
    }

    /// Check user's permissions of a file relative to a directory file descriptor
    /// TODO: check permissions based on uid/gid
    pub fn sys_faccessat(
        &self,
        dirfd: FileDesc,
        path: UserInPtr<u8>,
        mode: usize,
        flags: usize,
    ) -> SysResult {
        // TODO: check permissions based on uid/gid
        let path = path.as_c_str()?;
        let flags = AtFlags::from_bits_truncate(flags);
        info!(
            "faccessat: dirfd={:?}, path={:?}, mode={:#o}, flags={:?}",
            dirfd, path, mode, flags
        );
        let proc = self.linux_process();
        let follow = !flags.contains(AtFlags::SYMLINK_NOFOLLOW);
        let _inode = proc.lookup_inode_at(dirfd, path, follow)?;
        Ok(0)
    }

    /// change file timestamps with nanosecond precision
    pub fn sys_utimensat(
        &mut self,
        dirfd: FileDesc,
        pathname: UserInPtr<u8>,
        times: UserInOutPtr<[TimeSpec; 2]>,
        flags: usize,
    ) -> SysResult {
        info!(
            "utimensat(raw): dirfd: {:?}, pathname: {:?}, times: {:?}, flags: {:#x}",
            dirfd, pathname, times, flags
        );
        const UTIME_NOW: usize = 0x3fffffff;
        const UTIME_OMIT: usize = 0x3ffffffe;
        let proc = self.linux_process();
        let mut times = if times.is_null() {
            let epoch = TimeSpec::now();
            [epoch, epoch]
        } else {
            let times = times.read()?;
            [times[0], times[1]]
        };
        let inode = if pathname.is_null() {
            let fd = dirfd;
            info!("futimens: fd: {:?}, times: {:?}", fd, times);
            proc.get_file(fd)?.inode()
        } else {
            let pathname = pathname.as_c_str()?;
            info!(
                "utimensat: dirfd: {:?}, pathname: {:?}, times: {:?}, flags: {:#x}",
                dirfd, pathname, times, flags
            );
            let follow = if flags == 0 {
                true
            } else if flags == AtFlags::SYMLINK_NOFOLLOW.bits() {
                false
            } else {
                return Err(LxError::EINVAL);
            };
            proc.lookup_inode_at(dirfd, pathname, follow)?
        };
        let mut metadata = inode.metadata()?;
        if times[0].nsec != UTIME_OMIT {
            if times[0].nsec == UTIME_NOW {
                times[0] = TimeSpec::now();
            }
            metadata.atime = rcore_fs::vfs::Timespec {
                sec: times[0].sec as i64,
                nsec: times[0].nsec as i32,
            };
        }
        if times[1].nsec != UTIME_OMIT {
            if times[1].nsec == UTIME_NOW {
                times[1] = TimeSpec::now();
            }
            metadata.mtime = rcore_fs::vfs::Timespec {
                sec: times[1].sec as i64,
                nsec: times[1].nsec as i32,
            };
        }
        inode.set_metadata(&metadata)?;
        Ok(0)
    }

    /// Get filesystem statistics
    /// (see [linux man statfs(2)](https://man7.org/linux/man-pages/man2/statfs.2.html)).
    ///
    /// The `statfs` system call returns information about a mounted filesystem.
    /// `path` is the pathname of **any file** within the mounted filesystem.
    /// `buf` is a pointer to a `StatFs` structure.
    pub fn sys_statfs(&self, path: UserInPtr<u8>, mut buf: UserOutPtr<StatFs>) -> SysResult {
        let path = path.as_c_str()?;
        info!("statfs: path={:?}, buf={:?}", path, buf);

        // TODO
        // 现在 `path` 没用到，因为没实现真正的挂载，不可能搞一个非主要文件系统的路径。
        // 实现挂载之后，要用 `path` 分辨路径在哪个文件系统里，根据对应文件系统的特性返回统计信息。
        // （以及根据挂载选项填写 `StatFs::f_flags`！）

        let info = self.linux_process().root_inode().fs().info();
        buf.write(info.into())?;
        Ok(0)
    }

    /// Get filesystem statistics
    /// (see [linux man statfs(2)](https://man7.org/linux/man-pages/man2/statfs.2.html)).
    ///
    /// The `fstatfs` system call returns information about a mounted filesystem.
    /// `fd` is the descriptor referencing an open file.
    /// `buf` is a pointer to a `StatFs` structure.
    pub fn sys_fstatfs(&self, fd: FileDesc, mut buf: UserOutPtr<StatFs>) -> SysResult {
        info!("statfs: fd={:?}, buf={:?}", fd, buf);

        let info = self.linux_process().get_file(fd)?.inode().fs().info();
        buf.write(info.into())?;
        Ok(0)
    }
}

const F_LINUX_SPECIFIC_BASE: usize = 1024;

/// The file system statistics struct defined in linux
/// (see [linux man statfs(2)](https://man7.org/linux/man-pages/man2/statfs.2.html)).
#[repr(C)]
pub struct StatFs {
    f_type: i64,
    f_bsize: i64,
    f_blocks: u64,
    f_bfree: u64,
    f_bavail: u64,
    f_files: u64,
    f_ffree: u64,
    f_fsid: (i32, i32),
    f_namelen: isize,
    f_frsize: isize,
    f_flags: isize,
    f_spare: [isize; 4],
}

// 保证 `StatFs` 的定义和常见的 linux 一致
static_assertions::const_assert_eq!(120, core::mem::size_of::<StatFs>());

impl From<FsInfo> for StatFs {
    fn from(info: FsInfo) -> Self {
        StatFs {
            // TODO 文件系统的魔数，需要 rcore-fs 提供一个渠道获取
            // 但是这个似乎并没有什么用处，新的 vfs 相关函数都去掉了，也许永远填个常数就好了
            f_type: 0,
            f_bsize: info.bsize as _,
            f_blocks: info.blocks as _,
            f_bfree: info.bfree as _,
            f_bavail: info.bavail as _,
            f_files: info.files as _,
            f_ffree: info.ffree as _,
            // 一个由 OS 决定的号码，用于区分文件系统
            f_fsid: (0, 0),
            f_namelen: info.namemax as _,
            f_frsize: info.frsize as _,
            // TODO 需要先实现挂载
            f_flags: 0,
            f_spare: [0; 4],
        }
    }
}

numeric_enum_macro::numeric_enum! {
    #[repr(usize)]
    #[allow(non_camel_case_types)]
    #[derive(Eq, PartialEq, Debug, Copy, Clone)]
    /// fcntl flags
    pub enum FcntlCmd {
        /// dup
        DUPFD = 0,
        /// get close_on_exec
        GETFD = 1,
        /// set/clear close_on_exec
        SETFD = 2,
        /// get file->f_flags
        GETFL = 3,
        /// set file->f_flags
        SETFL = 4,
        /// Get record locking info.
        GETLK = 5,
        /// Set record locking info (non-blocking).
        SETLK = 6,
        /// Set record locking info (blocking).
        SETLKW = 7,
        /// like F_DUPFD, but additionally set the close-on-exec flag
        DUPFD_CLOEXEC = F_LINUX_SPECIFIC_BASE + 6,
    }
}
