//! Device tree parsing utilities for AArch64 / Apple Silicon / Asahi Linux.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;
use dtb_walker::{Dtb, DtbObj, HeaderError::*, Property, WalkOperation::*};

#[derive(Debug, Default, Clone)]
pub struct ParsedFdt {
    pub cmdline: Option<String>,
    pub memory_regions: Vec<Range<usize>>,
    pub uart_base: Option<usize>,
    pub gic_base: Option<usize>,
    pub aic_base: Option<usize>,
    pub aic_event_base: Option<usize>,
    /// AIC generation from `compatible`: `apple,aic` is 1, `apple,aic2` is 2,
    /// `apple,aic3` is 3.
    pub aic_version: Option<u32>,
    pub fb_base: Option<usize>,
    pub fb_size: Option<usize>,
    pub fb_width: Option<u32>,
    pub fb_height: Option<u32>,
    pub fb_stride: Option<u32>,
    pub fb_format: Option<String>,
}

/// Parses the DTB at the specified virtual/physical address.
pub fn parse_fdt(dtb_vaddr: usize) -> Option<ParsedFdt> {
    let dtb = unsafe {
        Dtb::from_raw_parts_filtered(dtb_vaddr as *const u8, |e| {
            matches!(e, Misaligned(4) | LastCompVersion(_))
        })
    }
    .ok()?;

    let mut result = ParsedFdt::default();

    dtb.walk(|path, obj| match obj {
        DtbObj::SubNode { name: _ } => StepInto,
        DtbObj::Property(prop) => {
            let path_name = path.name();
            let path_bytes = path_name.as_bytes();
            let path_str = core::str::from_utf8(path_bytes).unwrap_or("");

            match prop {
                Property::Compatible(comp_list) => {
                    for comp in comp_list {
                        let comp_str = comp.as_str().unwrap_or("");
                        if comp_str == "apple,aic"
                            || comp_str == "apple,aic2"
                            || comp_str.ends_with("-aic")
                            || comp_str.contains("aic")
                        {
                            result.aic_version = Some(if comp_str.ends_with("aic3") {
                                3
                            } else if comp_str.ends_with("aic2") {
                                2
                            } else {
                                1
                            });
                            if let Some(reg_prop) = find_reg_property(&dtb, path_bytes) {
                                if reg_prop.len() >= 16 {
                                    let base =
                                        u64::from_be_bytes(reg_prop[0..8].try_into().unwrap())
                                            as usize;
                                    result.aic_base = Some(base);
                                }
                                if reg_prop.len() >= 32 {
                                    let evt =
                                        u64::from_be_bytes(reg_prop[16..24].try_into().unwrap())
                                            as usize;
                                    result.aic_event_base = Some(evt);
                                }
                            }
                        } else if comp_str == "apple,s5l-uart" {
                            if let Some(reg_prop) = find_reg_property(&dtb, path_bytes) {
                                if reg_prop.len() >= 16 {
                                    let base =
                                        u64::from_be_bytes(reg_prop[0..8].try_into().unwrap())
                                            as usize;
                                    result.uart_base = Some(base);
                                }
                            }
                        } else if comp_str == "arm,pl011" {
                            if result.uart_base.is_none() {
                                if let Some(reg_prop) = find_reg_property(&dtb, path_bytes) {
                                    if reg_prop.len() >= 16 {
                                        let base =
                                            u64::from_be_bytes(reg_prop[0..8].try_into().unwrap())
                                                as usize;
                                        result.uart_base = Some(base);
                                    }
                                }
                            }
                        } else if comp_str == "arm,cortex-a15-gic" || comp_str == "arm,gic-400" {
                            if let Some(reg_prop) = find_reg_property(&dtb, path_bytes) {
                                if reg_prop.len() >= 16 {
                                    let base =
                                        u64::from_be_bytes(reg_prop[0..8].try_into().unwrap())
                                            as usize;
                                    result.gic_base = Some(base);
                                }
                            }
                        }
                    }
                }
                Property::General { name, value } => {
                    let name_str = name.as_str().unwrap_or("");
                    if name_str == "bootargs"
                        && (path_str == "chosen" || path_str.ends_with("/chosen"))
                    {
                        let val_str = core::str::from_utf8(value)
                            .unwrap_or("")
                            .trim_end_matches('\0');
                        result.cmdline = Some(String::from(val_str));
                    } else if path_str.starts_with("memory") {
                        if name_str == "reg" && value.len() >= 16 {
                            let (chunks, _) = value.as_chunks::<16>();
                            for chunk in chunks {
                                let addr =
                                    u64::from_be_bytes(chunk[0..8].try_into().unwrap()) as usize;
                                let size =
                                    u64::from_be_bytes(chunk[8..16].try_into().unwrap()) as usize;
                                if size > 0 {
                                    result.memory_regions.push(addr..addr + size);
                                }
                            }
                        }
                    } else if path_str.contains("framebuffer") {
                        if name_str == "reg" && value.len() >= 16 {
                            let addr = u64::from_be_bytes(value[0..8].try_into().unwrap()) as usize;
                            let size =
                                u64::from_be_bytes(value[8..16].try_into().unwrap()) as usize;
                            result.fb_base = Some(addr);
                            result.fb_size = Some(size);
                        } else if name_str == "width" && value.len() >= 4 {
                            result.fb_width =
                                Some(u32::from_be_bytes(value[0..4].try_into().unwrap()));
                        } else if name_str == "height" && value.len() >= 4 {
                            result.fb_height =
                                Some(u32::from_be_bytes(value[0..4].try_into().unwrap()));
                        } else if name_str == "stride" && value.len() >= 4 {
                            result.fb_stride =
                                Some(u32::from_be_bytes(value[0..4].try_into().unwrap()));
                        } else if name_str == "format" {
                            let s = core::str::from_utf8(value)
                                .unwrap_or("")
                                .trim_end_matches('\0');
                            result.fb_format = Some(String::from(s));
                        }
                    }
                }
                _ => {}
            }
            StepOver
        }
    });

    Some(result)
}

fn find_reg_property(dtb: &Dtb, target_node_bytes: &[u8]) -> Option<Vec<u8>> {
    let mut reg_vec = None;
    dtb.walk(|path, obj| match obj {
        DtbObj::SubNode { name: _ } => {
            let path_name = path.name();
            if path_name.as_bytes() == target_node_bytes {
                StepInto
            } else {
                StepOver
            }
        }
        DtbObj::Property(Property::General { name, value }) => {
            let path_name = path.name();
            if path_name.as_bytes() == target_node_bytes && name.as_bytes() == b"reg" {
                reg_vec = Some(Vec::from(value));
            }
            StepOver
        }
        _ => StepOver,
    });
    reg_vec
}
