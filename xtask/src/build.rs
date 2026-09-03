use crate::{errors::*, linux::LinuxRootfs, Arch, ArchArg, PROJECT_DIR};
use once_cell::sync::Lazy;
use os_xtask_utils::{dir, BinUtil, Cargo, CommandExt, Ext, Qemu};
use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    fs,
    path::PathBuf,
    str::FromStr,
};
use z_config::MachineConfig;

#[derive(Clone, Args)]
pub(crate) struct BuildArgs {
    /// Which machine is build for.
    #[clap(long, short)]
    pub machine: String,
    /// Build as debug mode.
    #[clap(long)]
    pub debug: bool,
    /// Extra features to enable.
    #[clap(long)]
    pub features: Option<String>,
}

#[derive(Args)]
pub(crate) struct OutArgs {
    #[clap(flatten)]
    build: BuildArgs,
    /// The file to save asm.
    #[clap(short, long)]
    output: Option<PathBuf>,
}

#[derive(Args)]
pub(crate) struct QemuArgs {
    #[clap(flatten)]
    arch: ArchArg,
    /// Which machine config to use.
    #[clap(long, short)]
    machine: Option<String>,
    /// Build as debug mode.
    #[clap(long)]
    debug: bool,
    /// Number of hart (SMP for Symmetrical Multiple Processor).
    #[clap(long)]
    smp: Option<u8>,
    /// Port for gdb to connect. If set, qemu will block and wait gdb to connect.
    #[clap(long)]
    gdb: Option<u16>,
    /// CPU model for QEMU.
    #[clap(long)]
    cpu: Option<String>,
    /// Extra features to enable.
    #[clap(long)]
    features: Option<String>,
}

#[derive(Args)]
pub(crate) struct GdbArgs {
    #[clap(flatten)]
    arch: ArchArg,
    #[clap(long)]
    port: u16,
}

static INNER: Lazy<PathBuf> = Lazy::new(|| PROJECT_DIR.join("zCore"));

pub(crate) struct BuildConfig {
    arch: Arch,
    debug: bool,
    env: HashMap<OsString, OsString>,
    features: HashSet<String>,
}

impl BuildConfig {
    pub fn from_args(args: BuildArgs) -> Result<Self, Report> {
        let machine = MachineConfig::select(&args.machine)
            .context(format!("Unknown target machine '{}'", args.machine))?;
        let mut features = HashSet::from_iter(machine.features.iter().cloned());
        if let Some(extra) = &args.features {
            for f in extra.split_whitespace() {
                features.insert(f.into());
            }
        }
        let mut env = HashMap::new();
        let arch = Arch::from_str(&machine.arch)
            .context(format!("Unknown arch {} for machine", machine.arch))?;
        // 递归 image
        if let Some(path) = &machine.user_img {
            features.insert("link-user-img".into());
            env.insert(
                "USER_IMG".into(),
                if path.is_absolute() {
                    path.as_os_str().to_os_string()
                } else {
                    PROJECT_DIR.join(path).as_os_str().to_os_string()
                },
            );
            LinuxRootfs::new(arch).image()?;
        }
        // 不支持 pci
        if !machine.pci_support {
            features.insert("no-pci".into());
        }
        if !features.contains("zircon") {
            features.insert("linux".into());
        }
        Ok(Self {
            arch,
            debug: args.debug,
            env,
            features,
        })
    }

    #[inline]
    fn target_file_path(&self) -> PathBuf {
        PROJECT_DIR
            .join("target")
            .join(self.arch.name())
            .join(if self.debug { "debug" } else { "release" })
            .join("zcore")
    }

    pub fn invoke(&self, cargo: impl FnOnce() -> Cargo) -> Result<(), Report> {
        let mut cargo = cargo();
        cargo
            .package("zcore")
            .features(false, &self.features)
            .target(INNER.join(format!("{}.json", self.arch.name())))
            .args(["-Z", "json-target-spec"])
            .args(["-Z", "build-std=core,alloc"])
            .args(["-Z", "build-std-features=compiler-builtins-mem"])
            .conditional(!self.debug, |cargo| {
                cargo.release();
            });
        for (key, val) in &self.env {
            println!("set build env: {key:?} : {val:?}");
            cargo.env(key, val);
        }
        cargo.run()
    }

    pub fn bin(&self, output: Option<PathBuf>) -> Result<PathBuf, Report> {
        // 递归 build
        self.invoke(Cargo::build)?;
        // 确定目录
        let obj = self.target_file_path();
        if self.arch == Arch::Riscv64 {
            let out = output.unwrap_or_else(|| obj.with_extension("bin"));
            // 生成
            println!("strip zcore to {}", out.display());
            dir::create_parent(&out).context(format!("Failed to create parent dir for {out:?}"))?;
            BinUtil::objcopy()
                .arg(format!("--binary-architecture={}", self.arch.name()))
                .arg(obj)
                .args(["--strip-all", "-O", "binary"])
                .arg(&out)
                .run()?;
            Ok(out)
        } else {
            Ok(obj)
        }
    }
}

impl OutArgs {
    /// 打印 asm。
    pub fn asm(self) -> Result<(), Report> {
        let Self { build, output } = self;
        let build = BuildConfig::from_args(build)?;
        // 递归 build
        build.invoke(Cargo::build)?;
        // 确定目录
        let obj = build.target_file_path();
        let out = output.unwrap_or_else(|| PROJECT_DIR.join("target/zcore.asm"));
        // 生成
        println!("Asm file dumps to '{}'.", out.display());
        dir::create_parent(&out).context(format!("Failed to create parent dir for {out:?}"))?;
        let output = BinUtil::objdump()
            .arg(obj)
            .arg("-d")
            .as_mut()
            .output()
            .context("Failed to run objdump")?;
        fs::write(&out, output.stdout).context(format!("Failed to write asm to {out:?}"))?;
        Ok(())
    }

    /// 生成 bin 文件。
    #[inline]
    pub fn bin(self) -> Result<PathBuf, Report> {
        let Self { build, output } = self;
        BuildConfig::from_args(build)?.bin(output)
    }
}

impl QemuArgs {
    /// 在 qemu 中启动。
    pub fn qemu(self) -> Result<(), Report> {
        // 递归 image
        self.arch.linux_rootfs().image()?;
        // 构造各种字符串
        let arch = self.arch.arch;
        let arch_str = arch.name();
        let obj = PROJECT_DIR
            .join("target")
            .join(self.arch.arch.name())
            .join(if self.debug { "debug" } else { "release" })
            .join("zcore");
        // 递归生成内核二进制
        let machine_name = self
            .machine
            .unwrap_or_else(|| format!("virt-{}", self.arch.arch.name()));
        let bin = BuildConfig::from_args(BuildArgs {
            machine: machine_name,
            debug: self.debug,
            features: self.features,
        })?
        .bin(None)?;
        // 设置 Qemu 参数
        let mut qemu = Qemu::system(arch_str);
        qemu.args(["-m", "4G"])
            .args(["-display", "none"])
            .arg("-no-reboot")
            .arg("-nographic")
            .optional(&self.smp, |qemu, smp| {
                qemu.args(["-smp", &smp.to_string()]);
            });
        match arch {
            Arch::Riscv64 => {
                qemu.args(["-machine", "virt"])
                    .args(["-bios", "default"])
                    .args(["-serial", "mon:stdio"]);
                if let Some(cpu) = &self.cpu {
                    qemu.args(["-cpu", cpu]);
                }
                qemu.arg("-kernel")
                    .arg(&bin)
                    .arg("-initrd")
                    .arg(INNER.join(format!("{arch_str}.img")))
                    .args(["-append", "\"LOG=warn\""]);
            }
            Arch::X86_64 => {
                let esp = INNER.join("esp");
                let efi_boot = esp.join("EFI").join("Boot");
                let efi_zcore = esp.join("EFI").join("zCore");
                dir::clear(&efi_boot).context(format!("Failed to clear dir {efi_boot:?}"))?;
                dir::clear(&efi_zcore).context(format!("Failed to clear dir {efi_zcore:?}"))?;

                let rboot_efi = PROJECT_DIR
                    .join("rboot")
                    .join("target")
                    .join("x86_64-unknown-uefi")
                    .join("release")
                    .join("rboot.efi");
                fs::copy(&rboot_efi, efi_boot.join("BootX64.efi"))
                    .context(format!("Failed to copy {rboot_efi:?} to BootX64.efi"))?;
                fs::copy(&obj, efi_zcore.join("zcore.elf"))
                    .context(format!("Failed to copy {obj:?} to zcore.elf"))?;
                fs::copy(INNER.join("x86_64.img"), efi_zcore.join("x86_64.img"))
                    .context("Failed to copy x86_64.img to esp/EFI/zCore/")?;
                fs::write(
                    efi_boot.join("rboot.conf"),
                    b"physical_memory_offset=0xFFFF800000000000\nkernel_path=\\EFI\\zCore\\zcore.elf\ninitramfs=\\EFI\\zCore\\x86_64.img\ncmdline=LOG=warn:ROOTPROC=/bin/busybox?sh\n",
                )
                .context("Failed to write rboot.conf")?;

                let ovmf = arch.target().join("firmware").join("OVMF.fd");
                let cpu = self
                    .cpu
                    .as_deref()
                    .unwrap_or("SandyBridge,+smap,-check,+fsgsbase");
                qemu.args(["-machine", "q35"])
                    .args(["-cpu", cpu])
                    .args(["-serial", "mon:stdio"])
                    .arg("-drive")
                    .arg(format!(
                        "format=raw,if=pflash,readonly=on,file={}",
                        ovmf.display()
                    ))
                    .arg("-drive")
                    .arg(format!("format=raw,file=fat:rw:{}", esp.display()))
                    .args(["-nic", "none"]);
            }
            Arch::Aarch64 => {
                fs::copy(&obj, INNER.join("disk").join("os"))
                    .context(format!("Failed to copy {obj:?} to disk/os"))?;
                let cpu = self.cpu.as_deref().unwrap_or("cortex-a53");
                qemu.args(["-machine", "virt"])
                    .args(["-cpu", cpu])
                    .arg("-bios")
                    .arg(arch.target().join("firmware").join("QEMU_EFI.fd"))
                    .args(["-hda", &format!("fat:rw:{}/disk", INNER.display())])
                    .args([
                        "-drive",
                        &format!(
                            "file={}/aarch64.img,if=none,format=raw,id=x0",
                            INNER.display()
                        ),
                    ])
                    .args([
                        "-device",
                        "virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0",
                    ]);
            }
        }
        qemu.optional(&self.gdb, |qemu, port| {
            qemu.args(["-S", "-gdb", &format!("tcp::{port}")]);
        })
        .run()
    }
}

impl GdbArgs {
    pub fn gdb(&self) -> Result<(), Report> {
        match self.arch.arch {
            Arch::Riscv64 => Ext::new("riscv64-unknown-elf-gdb")
                .args(["-ex", &format!("target remote localhost:{}", self.port)])
                .run(),
            Arch::Aarch64 => Ext::new("aarch64-none-linux-gnu-gdb")
                .args(["-ex", &format!("target remote localhost:{}", self.port)])
                .run(),
            Arch::X86_64 => bail!("GDB for x86_64 not yet implemented"),
        }
    }
}
