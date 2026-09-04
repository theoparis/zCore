//! Apple Device Tree (ADT) reader.
//!
//! m1n1 hands the kernel an `AppleBootArgs` with a pointer to iBoot's ADT, not
//! an FDT. The ADT is a little-endian, offset-free format: every node is
//!
//! ```text
//! u32 property_count; u32 child_count;
//! property_count * { char name[32]; u32 size; u8 value[align4(size)]; }
//! child_count * <node>
//! ```
//!
//! Device MMIO layouts are read from here rather than hardcoded: on AIC2 the
//! register-block layout (`extint-baseaddress` and the mask/config strides) is
//! die-specific, and guessing it puts stores into undecoded holes, which the
//! SoC fabric reports as an asynchronous SError. This mirrors `adt.c` /
//! `rust/src/adt.rs` and `aic23_init()` in m1n1.

/// Maximum path depth tracked for `reg` translation, matching m1n1's
/// `ADT_MAX_DEPTH`.
const MAX_DEPTH: usize = 8;

pub struct Adt<'a> {
    data: &'a [u8],
}

/// A node plus its ancestor chain, required to translate a `reg` entry through
/// each parent bus's `ranges`.
#[derive(Clone, Copy)]
pub struct NodePath {
    trace: [usize; MAX_DEPTH],
    depth: usize,
}

impl NodePath {
    pub fn offset(&self) -> usize {
        self.trace[self.depth - 1]
    }
}

impl<'a> Adt<'a> {
    /// # Safety
    /// `base` must point to `size` readable bytes containing an ADT.
    pub unsafe fn new(base: usize, size: usize) -> Option<Self> {
        // Every node header and property header is u32-aligned; an unaligned
        // base means the pointer is not an ADT.
        if base == 0 || !base.is_multiple_of(4) || size < 8 {
            return None;
        }
        let adt = Self {
            data: core::slice::from_raw_parts(base as *const u8, size),
        };
        // Sanity: the root must have properties and a plausible child count.
        let (props, children) = adt.counts(0)?;
        if props == 0 || props > 2048 || children == 0 || children > 2048 {
            return None;
        }
        Some(adt)
    }

    fn u32_at(&self, off: usize) -> Option<u32> {
        let bytes = self.data.get(off..off + 4)?;
        Some(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn counts(&self, node: usize) -> Option<(u32, u32)> {
        Some((self.u32_at(node)?, self.u32_at(node + 4)?))
    }

    /// Offset just past `node`'s properties, i.e. its first child.
    fn props_end(&self, node: usize) -> Option<usize> {
        let (props, _) = self.counts(node)?;
        let mut off = node + 8;
        for _ in 0..props {
            off = self.next_prop(off)?;
        }
        Some(off)
    }

    fn next_prop(&self, prop: usize) -> Option<usize> {
        let size = (self.u32_at(prop + 32)? & 0x7fff_ffff) as usize;
        let end = prop + 36 + ((size + 3) & !3);
        if end > self.data.len() {
            return None;
        }
        Some(end)
    }

    /// Offset just past `node` and its entire subtree, i.e. its next sibling.
    fn node_end(&self, node: usize) -> Option<usize> {
        let (_, children) = self.counts(node)?;
        let mut off = self.props_end(node)?;
        for _ in 0..children {
            off = self.node_end(off)?;
        }
        Some(off)
    }

    /// Raw value bytes of `node`'s property `name`.
    pub fn prop(&self, node: usize, name: &str) -> Option<&'a [u8]> {
        let (props, _) = self.counts(node)?;
        let mut off = node + 8;
        for _ in 0..props {
            let raw = self.data.get(off..off + 32)?;
            let end = raw.iter().position(|&b| b == 0).unwrap_or(32);
            if core::str::from_utf8(&raw[..end]).ok()? == name {
                let size = (self.u32_at(off + 32)? & 0x7fff_ffff) as usize;
                return self.data.get(off + 36..off + 36 + size);
            }
            off = self.next_prop(off)?;
        }
        None
    }

    pub fn prop_u32(&self, node: usize, name: &str) -> Option<u32> {
        let v = self.prop(node, name)?;
        Some(u32::from_le_bytes(v.get(..4)?.try_into().unwrap()))
    }

    /// True if `node`'s NUL-separated `compatible` list contains `compat`.
    pub fn is_compatible(&self, node: usize, compat: &str) -> bool {
        let Some(v) = self.prop(node, "compatible") else {
            return false;
        };
        v.split(|&b| b == 0)
            .filter_map(|s| core::str::from_utf8(s).ok())
            .any(|s| s == compat)
    }

    fn node_name(&self, node: usize) -> Option<&'a str> {
        let v = self.prop(node, "name")?;
        let end = v.iter().position(|&b| b == 0).unwrap_or(v.len());
        core::str::from_utf8(&v[..end]).ok()
    }

    /// ADT node names may carry a `@unit` suffix the caller omits.
    fn name_matches(actual: &str, wanted: &str) -> bool {
        actual == wanted
            || (actual.len() > wanted.len()
                && actual.as_bytes()[wanted.len()] == b'@'
                && actual.starts_with(wanted))
    }

    fn child_by_name(&self, node: usize, name: &str) -> Option<usize> {
        let (_, children) = self.counts(node)?;
        let mut off = self.props_end(node)?;
        for _ in 0..children {
            if Self::name_matches(self.node_name(off)?, name) {
                return Some(off);
            }
            off = self.node_end(off)?;
        }
        None
    }

    /// Resolves an absolute path such as `/arm-io/aic`, recording the ancestor
    /// chain needed by [`Adt::reg`].
    pub fn path(&self, path: &str) -> Option<NodePath> {
        let mut trace = [0usize; MAX_DEPTH];
        let mut depth = 0;
        let mut node = 0usize;
        for component in path.split('/').filter(|c| !c.is_empty()) {
            node = self.child_by_name(node, component)?;
            if depth == MAX_DEPTH {
                return None;
            }
            trace[depth] = node;
            depth += 1;
        }
        if depth == 0 {
            return None;
        }
        Some(NodePath { trace, depth })
    }

    fn cells(v: &[u8]) -> u64 {
        let mut val = 0u64;
        for (i, chunk) in v.as_chunks::<4>().0.iter().enumerate().take(2) {
            val |= (u32::from_le_bytes(*chunk) as u64) << (32 * i);
        }
        val
    }

    /// Reads `reg` entry `index` of the node at `path`, translating the address
    /// through every ancestor's `ranges`. Returns `(addr, size)`.
    pub fn reg(&self, path: &NodePath, index: usize) -> Option<(u64, u64)> {
        let mut cursor = path.depth - 1;
        let mut node = path.trace[cursor];
        let mut parent = if cursor > 0 {
            path.trace[cursor - 1]
        } else {
            0
        };

        let mut addr_cells = self.prop_u32(parent, "#address-cells")? as usize;
        let mut size_cells = self.prop_u32(parent, "#size-cells")? as usize;
        if !(1..=2).contains(&addr_cells) || size_cells > 2 {
            return None;
        }

        let reg = self.prop(node, "reg")?;
        let stride = (addr_cells + size_cells) * 4;
        let entry = reg.get(index * stride..(index + 1) * stride)?;
        let mut addr = Self::cells(&entry[..addr_cells * 4]);
        let size = Self::cells(&entry[addr_cells * 4..]);

        // Walk up, remapping through each bus's `ranges` until the root.
        while parent != 0 || cursor > 0 {
            node = parent;
            cursor -= 1;
            parent = if cursor > 0 {
                path.trace[cursor - 1]
            } else {
                0
            };

            let Some(ranges) = self.prop(node, "ranges") else {
                break;
            };
            let parent_addr_cells = self.prop_u32(parent, "#address-cells")? as usize;
            if !(1..=2).contains(&parent_addr_cells) {
                return None;
            }
            let entry_size = (parent_addr_cells + addr_cells + size_cells) * 4;
            for range in ranges.chunks_exact(entry_size) {
                let (child, rest) = range.split_at(addr_cells * 4);
                let (parent_base, child_size) = rest.split_at(parent_addr_cells * 4);
                let child = Self::cells(child);
                let child_size = Self::cells(child_size);
                if addr >= child && addr + size <= child + child_size {
                    addr = addr - child + Self::cells(parent_base);
                    break;
                }
            }
            size_cells = self.prop_u32(parent, "#size-cells")? as usize;
            addr_cells = parent_addr_cells;
            if cursor == 0 {
                break;
            }
        }

        Some((addr, size))
    }

    /// Convenience: `reg` entry `index` of `path`, or `None` if either is absent.
    pub fn path_reg(&self, path: &str, index: usize) -> Option<(u64, u64)> {
        let node = self.path(path)?;
        self.reg(&node, index)
    }
}
