mod image;
mod opencv;
mod test;

use crate::{commands::fetch_online, errors::*, Arch, PROJECT_DIR, REPOS};
use os_xtask_utils::{dir, CommandExt, Ext, Git, Make};
use std::{
    env,
    ffi::OsString,
    fs,
    os::unix,
    path::{Path, PathBuf},
};

pub(crate) struct LinuxRootfs(Arch);

impl LinuxRootfs {
    /// 生成指定架构的 linux rootfs 操作对象。
    #[inline]
    pub const fn new(arch: Arch) -> Self {
        Self(arch)
    }

    /// 构造启动内存文件系统 rootfs。
    /// 对于 x86_64，这个文件系统可用于 libos 启动。
    /// 若设置 `clear`，将清除已存在的目录。
    pub fn make(&self, clear: bool) -> Result<(), Report> {
        // 若已存在且不需要清空，可以直接退出
        let dir = self.path();
        if dir.is_dir() && !clear {
            return Ok(());
        }
        // 准备最小系统需要的资源
        let musl = self.0.linux_musl_cross()?;
        let busybox = self.busybox(&musl)?;
        // 创建目标目录
        let bin = dir.join("bin");
        let lib = dir.join("lib");
        dir::clear(&dir).context(format!("Failed to clear rootfs directory {dir:?}"))?;
        fs::create_dir_all(&bin).context(format!("Failed to create directory {bin:?}"))?;
        fs::create_dir_all(&lib).context(format!("Failed to create directory {lib:?}"))?;
        // 拷贝 busybox
        fs::copy(&busybox, bin.join("busybox")).context(format!(
            "Failed to copy busybox from {busybox:?} to {:?}",
            bin.join("busybox")
        ))?;
        // 拷贝 libc.so
        let from = musl
            .join(format!("{}-linux-musl", self.0.name()))
            .join("lib")
            .join("libc.so");
        let to = lib.join(format!("ld-musl-{arch}.so.1", arch = self.0.name()));
        fs::copy(&from, &to).context(format!("Failed to copy libc.so from {from:?} to {to:?}"))?;
        Ext::new(self.strip(&musl)).arg("-s").arg(&to).run()?;
        // 为常用功能建立符号链接
        const SH: &[&str] = &[
            "cat", "cp", "echo", "false", "grep", "gzip", "kill", "ln", "ls", "mkdir", "mv",
            "pidof", "ping", "ping6", "printenv", "ps", "pwd", "rm", "rmdir", "sh", "sleep",
            "stat", "tar", "touch", "true", "uname", "usleep", "watch",
        ];
        let bin = dir.join("bin");
        for sh in SH {
            let link = bin.join(sh);
            let _ = fs::remove_file(&link);
            unix::fs::symlink("busybox", &link)
                .context(format!("Failed to create symlink for {link:?}"))?;
        }
        Ok(())
    }

    /// 将 musl 动态库放入 rootfs。
    pub fn put_musl_libs(&self) -> Result<PathBuf, Report> {
        // 递归 rootfs
        self.make(false)?;
        let dir = self.0.linux_musl_cross()?;
        self.put_libs(&dir, dir.join(format!("{}-linux-musl", self.0.name())))?;
        Ok(dir)
    }

    /// 指定架构的 rootfs 路径。
    #[inline]
    pub fn path(&self) -> PathBuf {
        PROJECT_DIR.join("rootfs").join(self.0.name())
    }

    /// 编译 busybox。
    fn busybox(&self, musl: impl AsRef<Path>) -> Result<PathBuf, Report> {
        // 最终文件路径
        let target = self.0.target().join("busybox");
        // 如果文件存在，直接退出
        let executable = target.join("busybox");
        if executable.is_file() {
            return Ok(executable);
        }
        // 获得源码
        let source = REPOS.join("busybox");
        if !source.is_dir() {
            fetch_online!(source, |tmp| {
                Git::clone("https://git.busybox.net/busybox.git")
                    .dir(tmp)
                    .single_branch()
                    .depth(1)
                    .done()
            })?;
        }
        // 拷贝
        let _ = dir::rm(&target);
        dircpy::copy_dir(source, &target)
            .context(format!("Failed to copy busybox source to {target:?}"))?;
        // 配置
        Make::new().current_dir(&target).arg("defconfig").run()?;
        // 编译
        let musl = musl.as_ref();
        let mut make = Make::new();
        make.current_dir(&target);

        let musl_canonical = musl
            .canonicalize()
            .context(format!("Failed to canonicalize {musl:?}"))?;
        let gcc_path = musl_canonical
            .join("bin")
            .join(format!("{}-linux-musl-gcc", self.0.name()));

        if cfg!(not(target_os = "linux")) || !gcc_path.is_file() {
            let sysroot = musl_canonical.join(format!("{}-linux-musl", self.0.name()));
            let cc = format!(
                "clang --target={arch}-linux-musl --sysroot={sysroot} --gcc-toolchain={musl} -fuse-ld=lld",
                arch = self.0.name(),
                sysroot = sysroot.display(),
                musl = musl_canonical.display(),
            );
            make.arg(format!("CC={cc}"))
                .arg("AR=llvm-ar")
                .arg("STRIP=llvm-strip")
                .arg("HOSTCC=clang");
        } else {
            make.arg(format!(
                "CROSS_COMPILE={musl}/{arch}-linux-musl-",
                musl = musl_canonical.join("bin").display(),
                arch = self.0.name(),
            ))
            .arg("EXTRA_CFLAGS=-Wl,-z,max-page-size=65536")
            .arg("EXTRA_LDFLAGS=-Wl,-z,max-page-size=65536");
        }
        make.run()?;
        // 裁剪
        Ext::new(self.strip(musl))
            .arg("-s")
            .arg(&executable)
            .run()?;
        Ok(executable)
    }

    fn strip(&self, musl: impl AsRef<Path>) -> PathBuf {
        let musl_strip = musl
            .as_ref()
            .join("bin")
            .join(format!("{}-linux-musl-strip", self.0.name()));
        if cfg!(not(target_os = "linux"))
            && std::process::Command::new("llvm-strip")
                .arg("--version")
                .output()
                .is_ok()
        {
            return PathBuf::from("llvm-strip");
        }
        if musl_strip.is_file() {
            musl_strip
        } else if std::process::Command::new("llvm-strip")
            .arg("--version")
            .output()
            .is_ok()
        {
            PathBuf::from("llvm-strip")
        } else {
            PathBuf::from(format!("{}-linux-musl-strip", self.0.name()))
        }
    }

    /// 从安装目录拷贝所有 so 和 so 链接到 rootfs
    fn put_libs(&self, musl: impl AsRef<Path>, dir: impl AsRef<Path>) -> Result<(), Report> {
        let lib = self.path().join("lib");
        let musl_libc_protected = format!("ld-musl-{}.so.1", self.0.name());
        let musl_libc_ignored = "libc.so";
        let strip = self.strip(musl);
        let entries = dir.as_ref().join("lib").read_dir().context(format!(
            "Failed to read lib dir {:?}",
            dir.as_ref().join("lib")
        ))?;
        for entry in entries.filter_map(Result::ok) {
            let source = entry.path();
            if !check_so(&source) {
                continue;
            }
            let name = source
                .file_name()
                .context(format!("Missing file name for {source:?}"))?;
            let target = lib.join(name);
            if source.is_symlink() {
                if name != musl_libc_protected.as_str() {
                    let _ = dir::rm(&target);
                    let target_link = source
                        .read_link()
                        .context(format!("Failed to read link for {source:?}"))?;
                    unix::fs::symlink(target_link, &target)
                        .context(format!("Failed to create symlink {target:?}"))?;
                }
            } else if name != musl_libc_ignored {
                let _ = dir::rm(&target);
                fs::copy(&source, &target)
                    .context(format!("Failed to copy {source:?} to {target:?}"))?;
                let _ = Ext::new(&strip).arg("-s").arg(&target).run();
            }
        }
        Ok(())
    }
}

/// 为 PATH 环境变量附加路径。
fn join_path_env<I, S>(paths: I) -> OsString
where
    I: IntoIterator<Item = S>,
    S: AsRef<Path>,
{
    let mut path = OsString::new();
    let mut first = true;
    if let Ok(current) = env::var("PATH") {
        path.push(current);
        first = false;
    }
    for item in paths {
        if first {
            first = false;
        } else {
            path.push(":");
        }
        path.push(item.as_ref().canonicalize().unwrap().as_os_str());
    }
    path
}

/// 判断一个文件是动态库或动态库的符号链接。
fn check_so<P: AsRef<Path>>(path: P) -> bool {
    let path = path.as_ref();
    // 是符号链接或文件
    // 对于符号链接，`is_file` `exist` 等函数都会针对其指向的真实文件判断
    if !path.is_symlink() && !path.is_file() {
        return false;
    }
    // 对文件名分段
    let name = path.file_name().unwrap().to_string_lossy();
    let mut seg = name.split('.');
    // 不能以 . 开头
    if matches!(seg.next(), Some("") | None) {
        return false;
    }
    // 扩展名的第一项是 so
    if !matches!(seg.next(), Some("so")) {
        return false;
    }
    // so 之后全是纯十进制数字
    !seg.any(|it| !it.chars().all(|ch| ch.is_ascii_digit()))
}
