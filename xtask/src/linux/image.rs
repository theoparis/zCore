use crate::{commands::wget, errors::*, Arch, PROJECT_DIR};
use os_xtask_utils::{dir, CommandExt, Ext, Qemu};
use std::{fs, path::Path};

impl super::LinuxRootfs {
    /// 生成镜像。
    pub fn image(&self) -> Result<(), Report> {
        // 递归 rootfs
        self.make(false)?;
        // 镜像路径
        let inner = PROJECT_DIR.join("zCore");
        let image = inner.join(format!("{arch}.img", arch = self.0.name()));
        // aarch64 升级为 rboot
        if let Arch::Aarch64 = self.0 {
            // 编译 vendored rboot 引导程序
            os_xtask_utils::Cargo::build()
                .arg("--manifest-path")
                .arg(PROJECT_DIR.join("rboot").join("Cargo.toml"))
                .arg("--target")
                .arg("aarch64-unknown-uefi")
                .args(["-Z", "build-std=core,alloc"])
                .args(["-Z", "build-std-features=compiler-builtins-mem"])
                .arg("--release")
                .run()?;

            let boot_dir = inner.join("disk").join("EFI").join("Boot");
            dir::clear(&boot_dir).unwrap();

            Ext::new("cargo")
                .arg("build")
                .arg("--manifest-path")
                .arg("rboot/Cargo.toml")
                .arg("--target")
                .arg("aarch64-unknown-uefi")
                .arg("-Zbuild-std=core,alloc")
                .arg("-Zbuild-std-features=compiler-builtins-mem")
                .arg("--release")
                .invoke();

            let rboot_efi = PROJECT_DIR
                .join("rboot")
                .join("target")
                .join("aarch64-unknown-uefi")
                .join("release")
                .join("rboot.efi");

            fs::copy(&rboot_efi, boot_dir.join("bootaa64.efi")).unwrap();
            fs::write(
                boot_dir.join("rboot.conf"),
                b"physical_memory_offset=0xFFFF800000000000\nkernel_path=\\os\ncmdline=LOG=warn:ROOTPROC=/bin/busybox?sh\n",
            ).unwrap();
        }
        // x86_64 还需要下载 OVMF.fd 并编译 rboot
        if let Arch::X86_64 = self.0 {
            const URL: &str = "https://github.com/retrage/edk2-nightly/raw/e32f6c3dedd0dab3f25a8665b88a53c9cf2941d9/bin/DEBUGX64_OVMF.fd";
            let fw_dir = self.0.target().join("firmware");
            let ovmf_path = fw_dir.join("OVMF.fd");
            if !ovmf_path.exists() {
                dir::create_parent(&ovmf_path)
                    .context(format!("Failed to create parent dir for {ovmf_path:?}"))?;
                wget(URL, &ovmf_path)?;
            }

            // 编译 vendored rboot 引导程序
            os_xtask_utils::Cargo::build()
                .arg("--manifest-path")
                .arg(PROJECT_DIR.join("rboot").join("Cargo.toml"))
                .arg("--target")
                .arg("x86_64-unknown-uefi")
                .args(["-Z", "build-std=core,alloc"])
                .args(["-Z", "build-std-features=compiler-builtins-mem"])
                .arg("--release")
                .run()?;
        }
        // 生成镜像
        fuse(self.path(), &image)?;
        // 扩充一些额外空间，供某些测试使用
        if std::process::Command::new("qemu-img")
            .arg("--version")
            .output()
            .is_ok()
        {
            Qemu::img()
                .arg("resize")
                .args(["-f", "raw"])
                .arg(&image)
                .arg("+5M")
                .run()?;
        } else {
            let file = fs::OpenOptions::new()
                .write(true)
                .open(&image)
                .context(format!("Failed to open image {image:?}"))?;
            let len = file
                .metadata()
                .context("Failed to get image metadata")?
                .len();
            file.set_len(len + 5 * 1024 * 1024)
                .context("Failed to resize image")?;
        }
        Ok(())
    }
}

/// 制作镜像。
fn fuse(dir: impl AsRef<Path>, image: impl AsRef<Path>) -> Result<(), Report> {
    use rcore_fs::vfs::FileSystem;
    use rcore_fs_fuse::zip::zip_dir;
    use rcore_fs_sfs::SimpleFileSystem;
    use std::sync::{Arc, Mutex};

    let dir = dir.as_ref();
    let image = image.as_ref();
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(image)
        .context(format!("Failed to open or create image file {image:?}"))?;
    const MAX_SPACE: usize = 1024 * 1024 * 1024; // 1GiB
    let fs = SimpleFileSystem::create(Arc::new(Mutex::new(file)), MAX_SPACE)
        .map_err(|e| report!("Failed to create simple file system: {:?}", e))?;
    zip_dir(dir, fs.root_inode())
        .map_err(|e| report!("Failed to zip directory {:?} into rootfs: {:?}", dir, e))?;
    Ok(())
}
