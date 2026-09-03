use super::join_path_env;
use crate::{commands::wget, errors::*, Arch};
use os_xtask_utils::{dir, CommandExt, Ext, Make, Tar};
use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    fs,
    path::PathBuf,
};

impl super::LinuxRootfs {
    /// 将 libc-test 放入 rootfs。
    pub fn put_libc_test(&self) -> Result<(), Report> {
        // 递归 rootfs
        self.make(false)?;
        // 拷贝仓库
        let dir = self.path().join("libc-test");
        let _ = dir::rm(&dir);
        dircpy::copy_dir("libc-test", &dir)
            .context(format!("Failed to copy libc-test to {dir:?}"))?;
        // 编译
        fs::copy(dir.join("config.mak.def"), dir.join("config.mak"))
            .context("Failed to copy config.mak.def to config.mak")?;
        let musl_cross = self.0.linux_musl_cross()?;
        Make::new()
            .j(usize::MAX)
            .env("ARCH", self.0.name())
            .env("CROSS_COMPILE", format!("{}-linux-musl-", self.0.name()))
            .env("PATH", join_path_env(&[musl_cross.join("bin")]))
            .current_dir(&dir)
            .run()?;
        // FIXME 为什么要替换？
        if let Arch::Riscv64 = self.0 {
            fs::copy(
                riscv64_special()?.join("libc-test/functional/tls_align-static.exe"),
                dir.join("src/functional/tls_align-static.exe"),
            )
            .context("Failed to replace tls_align-static.exe for riscv64")?;
        }

        // 删除 libc-test 不必要的文件
        let elf_path = OsString::from("src");
        let test_set = HashSet::from([
            OsString::from("api"),
            OsString::from("common"),
            OsString::from("math"),
            OsString::from("musl"),
            OsString::from("functional"),
            OsString::from("regression"),
        ]);

        if let Ok(entries) = fs::read_dir(&dir) {
            entries
                .filter_map(Result::ok)
                .filter(|path| path.file_name() != elf_path)
                .for_each(|path| {
                    let _ = dir::rm(path.path());
                });
        }

        if let Ok(entries) = fs::read_dir(dir.join(&elf_path)) {
            entries
                .filter_map(Result::ok)
                .filter(|path| !test_set.contains(&path.file_name()))
                .for_each(|path| {
                    let _ = dir::rm(path.path());
                });
        }

        for item in test_set {
            if let Ok(entries) = fs::read_dir(dir.join(&elf_path).join(item)) {
                entries
                    .filter_map(Result::ok)
                    .filter(|path| {
                        let name = path.file_name().to_string_lossy().to_string();
                        !name.ends_with(".exe") && !name.ends_with(".so")
                    })
                    .for_each(|path| {
                        let _ = dir::rm(path.path());
                    });
            }
        }
        Ok(())
    }

    /// 将其他测试放入 rootfs。
    pub fn put_other_test(&self) -> Result<(), Report> {
        // 递归 rootfs
        self.make(false)?;
        // build linux-syscall/test
        let bin = self.path().join("bin");
        let musl = self.0.linux_musl_cross()?;
        let musl_canonical = musl
            .canonicalize()
            .context("Failed to canonicalize musl path")?;
        let gcc_path = musl_canonical
            .join("bin")
            .join(format!("{}-linux-musl-gcc", self.0.name()));
        let use_clang = cfg!(not(target_os = "linux")) || !gcc_path.is_file();

        let entries = fs::read_dir("linux-syscall/test")
            .context("Failed to read linux-syscall/test directory")?;
        for entry in entries.filter_map(|res| res.ok()) {
            let c = entry.path();
            if c.extension() != Some(OsStr::new("c")) {
                continue;
            }
            let stem = c.file_stem().context("Missing file stem")?;
            let output_bin = bin.join(stem);
            if use_clang {
                let sysroot = musl_canonical.join(format!("{}-linux-musl", self.0.name()));
                Ext::new("clang")
                    .arg(format!("--target={}-linux-musl", self.0.name()))
                    .arg(format!("--sysroot={}", sysroot.display()))
                    .arg(format!("--gcc-toolchain={}", musl_canonical.display()))
                    .arg("-fuse-ld=lld")
                    .arg("-D_GNU_SOURCE")
                    .arg("-Wno-implicit-function-declaration")
                    .arg(&c)
                    .arg("-o")
                    .arg(&output_bin)
                    .run()?;
            } else {
                Ext::new(&gcc_path)
                    .arg(&c)
                    .arg("-o")
                    .arg(&output_bin)
                    .run()?;
            }
        }
        // 再为 riscv64 添加 oscomp
        if let Arch::Riscv64 = self.0 {
            let oscomp_dir = riscv64_special()?.join("oscomp");
            dircpy::copy_dir(oscomp_dir, self.path().join("oscomp"))
                .context("Failed to copy oscomp dir")?;
        }
        Ok(())
    }
}

fn riscv64_special() -> Result<PathBuf, Report> {
    const URL: &str =
        "https://github.com/rcore-os/libc-test-prebuilt/releases/download/0.1/prebuild.tar.xz";
    let tar = Arch::Riscv64.origin().join("prebuild.tar.xz");
    wget(URL, &tar)?;
    // 解压到目标路径
    let dir = Arch::Riscv64.target();
    dir::clear(&dir).context(format!("Failed to clear dir {dir:?}"))?;
    Tar::xf(&tar, Some(&dir)).run()?;
    Ok(dir.join("prebuild"))
}
