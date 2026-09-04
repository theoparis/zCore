#![cfg_attr(not(feature = "libos"), no_std)]
#![deny(warnings)]
#![allow(unexpected_cfgs)]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};

extern crate alloc;
#[macro_use]
extern crate log;
#[macro_use]
extern crate cfg_if;

#[macro_use]
mod logging;

#[cfg(not(feature = "libos"))]
mod lang;

mod fs;
mod handler;
mod platform;
mod utils;

cfg_if! {
    if #[cfg(target_arch = "x86_64")] {
        #[path = "memory_x86_64.rs"]
        mod memory;
    } else {
        mod memory;
    }
}

static STARTED: AtomicBool = AtomicBool::new(false);

#[cfg(all(not(any(feature = "libos")), feature = "mock-disk"))]
static MOCK_CORE: AtomicBool = AtomicBool::new(false);

/// Reports boot progress over the raw m1n1 VUART. `println!`/`info!` only work
/// once `primary_init_early` has registered a UART driver, so early stages are
/// otherwise invisible.
#[cfg(all(feature = "board-apple", not(feature = "libos")))]
fn stage(msg: &str) {
    platform::early_print(msg);
}

#[cfg(not(all(feature = "board-apple", not(feature = "libos"))))]
fn stage(_msg: &str) {}

fn primary_main(config: kernel_hal::KernelConfig) {
    stage("HVLOG: stage: logging\n");
    logging::init();
    stage("HVLOG: stage: memory\n");
    memory::init();
    stage("HVLOG: stage: primary_init_early\n");
    kernel_hal::primary_init_early(config, &handler::ZcoreKernelHandler);
    stage("HVLOG: stage: boot_options\n");
    let options = utils::boot_options();
    logging::set_max_level(&options.log_level);
    info!("Boot options: {:#?}", options);
    stage("HVLOG: stage: insert_regions\n");
    memory::insert_regions(&kernel_hal::mem::free_pmem_regions());
    stage("HVLOG: stage: primary_init\n");
    kernel_hal::primary_init();
    stage("HVLOG: stage: run root proc\n");
    STARTED.store(true, Ordering::SeqCst);
    cfg_if! {
        if #[cfg(all(feature = "linux", feature = "zircon"))] {
            panic!("Feature `linux` and `zircon` cannot be enabled at the same time!");
        } else if #[cfg(feature = "linux")] {
            let args = options.root_proc.split('?').map(Into::into).collect(); // parse "arg0?arg1?arg2"
            let envs = alloc::vec!["PATH=/usr/sbin:/usr/bin:/sbin:/bin".into()];
            let rootfs = fs::rootfs();
            let proc = zcore_loader::linux::run(args, envs, rootfs);
            utils::wait_for_exit(Some(proc))
        } else if #[cfg(feature = "zircon")] {
            let zbi = fs::zbi();
            let proc = zcore_loader::zircon::run_userboot(zbi, &options.cmdline);
            utils::wait_for_exit(Some(proc))
        } else {
            panic!("One of the features `linux` or `zircon` must be specified!");
        }
    }
}

#[cfg(not(any(feature = "libos", target_arch = "aarch64")))]
fn secondary_main() -> ! {
    while !STARTED.load(Ordering::SeqCst) {
        core::hint::spin_loop();
    }
    kernel_hal::secondary_init();
    info!("hart{} inited", kernel_hal::cpu::cpu_id());
    #[cfg(feature = "mock-disk")]
    {
        if MOCK_CORE
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            utils::mock_disk();
        }
    }
    utils::wait_for_exit(None)
}
