//! The host-side editor: an owned tree that parses from and serializes to a version-17 DTB.

use std::{collections::HashMap, string::String, vec::Vec};

use crate::{Error, Fdt, HEADER_SIZE, MAGIC, MAX_DEPTH, Token};

const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_END: u32 = 9;

/// An owned property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropBuf {
    pub name: String,
    pub value: Vec<u8>,
}

/// An owned node.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NodeBuf {
    pub name: String,
    pub props: Vec<PropBuf>,
    pub children: Vec<NodeBuf>,
}

impl NodeBuf {
    pub fn new(name: &str) -> Self {
        NodeBuf {
            name: name.into(),
            ..Default::default()
        }
    }

    pub fn prop(&self, name: &str) -> Option<&[u8]> {
        self.props
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.value.as_slice())
    }

    /// Replaces the property in place, or appends it.
    pub fn set_prop(&mut self, name: &str, value: Vec<u8>) {
        match self.props.iter_mut().find(|p| p.name == name) {
            Some(p) => p.value = value,
            None => self.props.push(PropBuf {
                name: name.into(),
                value,
            }),
        }
    }

    pub fn remove_prop(&mut self, name: &str) -> bool {
        let before = self.props.len();
        self.props.retain(|p| p.name != name);
        self.props.len() != before
    }

    pub fn child(&self, name: &str) -> Option<&NodeBuf> {
        self.children.iter().find(|c| c.name == name)
    }

    /// A one-cell property as u32.
    pub fn prop_u32(&self, name: &str) -> Result<Option<u32>, Error> {
        match self.prop(name) {
            None => Ok(None),
            Some(v) => {
                let b: [u8; 4] = v.try_into().map_err(|_| Error::BadLength)?;
                Ok(Some(u32::from_be_bytes(b)))
            }
        }
    }
}

/// An owned device tree.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tree {
    pub boot_cpuid_phys: u32,
    /// `/memreserve/` entries `(address, size)`.
    pub reservations: Vec<(u64, u64)>,
    pub root: NodeBuf,
}

fn components(path: &str) -> Result<Vec<&str>, Error> {
    let rest = path.strip_prefix('/').ok_or(Error::BadPath)?;
    if rest.is_empty() {
        return Ok(Vec::new());
    }
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let comps: Vec<&str> = rest.split('/').collect();
    if comps.iter().any(|c| c.is_empty() || c.contains('\0')) {
        return Err(Error::BadPath);
    }
    Ok(comps)
}

/// 32-bit big-endian cells.
pub fn cells32(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_be_bytes()).collect()
}

/// `value` as `cells` (1 or 2) big-endian cells; fails when it does not fit.
pub fn cells(value: u64, cells: u32) -> Result<Vec<u8>, Error> {
    match cells {
        1 => Ok(u32::try_from(value)
            .map_err(|_| Error::BadCells)?
            .to_be_bytes()
            .to_vec()),
        2 => Ok(value.to_be_bytes().to_vec()),
        _ => Err(Error::BadCells),
    }
}

/// A NUL-terminated string value.
pub fn string(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

impl Tree {
    /// Parses a DTB (validated by [`Fdt::new`]) into an owned tree.
    pub fn parse(bytes: &[u8]) -> Result<Tree, Error> {
        let fdt = Fdt::new(bytes)?;
        let mut stack: Vec<NodeBuf> = Vec::new();
        let mut root = None;
        for token in fdt.tokens() {
            match token? {
                Token::BeginNode(name) => {
                    if stack.len() >= MAX_DEPTH {
                        return Err(Error::TooDeep);
                    }
                    stack.push(NodeBuf::new(name));
                }
                Token::Prop(p) => {
                    stack
                        .last_mut()
                        .ok_or(Error::BadStructure)?
                        .props
                        .push(PropBuf {
                            name: p.name.into(),
                            value: p.value.to_vec(),
                        })
                }
                Token::EndNode => {
                    let done = stack.pop().ok_or(Error::BadStructure)?;
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(done),
                        None => root = Some(done),
                    }
                }
                Token::Nop | Token::End => {}
            }
        }
        Ok(Tree {
            boot_cpuid_phys: fdt.boot_cpuid_phys(),
            reservations: fdt.mem_reservations().collect(),
            root: root.ok_or(Error::BadStructure)?,
        })
    }

    pub fn find(&self, path: &str) -> Result<Option<&NodeBuf>, Error> {
        let mut cur = &self.root;
        for c in components(path)? {
            match cur.child(c) {
                Some(n) => cur = n,
                None => return Ok(None),
            }
        }
        Ok(Some(cur))
    }

    pub fn find_mut(&mut self, path: &str) -> Result<Option<&mut NodeBuf>, Error> {
        let mut cur = &mut self.root;
        for c in components(path)? {
            match cur.children.iter().position(|n| n.name == c) {
                Some(i) => cur = &mut cur.children[i],
                None => return Ok(None),
            }
        }
        Ok(Some(cur))
    }

    /// Creates the node at `path` and any missing parents; returns the existing node if there is one.
    pub fn add_node(&mut self, path: &str) -> Result<&mut NodeBuf, Error> {
        let comps = components(path)?;
        if comps.len() > MAX_DEPTH {
            return Err(Error::TooDeep);
        }
        let mut cur = &mut self.root;
        for c in comps {
            let i = match cur.children.iter().position(|n| n.name == c) {
                Some(i) => i,
                None => {
                    cur.children.push(NodeBuf::new(c));
                    cur.children.len() - 1
                }
            };
            cur = &mut cur.children[i];
        }
        Ok(cur)
    }

    /// Adds or replaces property `name` of the existing node at `path`.
    pub fn set_prop(&mut self, path: &str, name: &str, value: Vec<u8>) -> Result<(), Error> {
        if name.is_empty() || name.contains('\0') {
            return Err(Error::BadString);
        }
        self.find_mut(path)?
            .ok_or(Error::NotFound)?
            .set_prop(name, value);
        Ok(())
    }

    /// Deletes a property; returns whether it existed. The node must exist.
    pub fn remove_prop(&mut self, path: &str, name: &str) -> Result<bool, Error> {
        Ok(self
            .find_mut(path)?
            .ok_or(Error::NotFound)?
            .remove_prop(name))
    }

    /// Deletes a node and its subtree; returns whether it existed. The root cannot be deleted.
    pub fn remove_node(&mut self, path: &str) -> Result<bool, Error> {
        let mut comps = components(path)?;
        let last = comps.pop().ok_or(Error::BadPath)?;
        let parent_path = format!("/{}", comps.join("/"));
        let Some(parent) = self.find_mut(&parent_path)? else {
            return Ok(false);
        };
        let before = parent.children.len();
        parent.children.retain(|n| n.name != last);
        Ok(parent.children.len() != before)
    }

    /// Serializes to a version-17 DTB: header, reservation block, structure block, strings block.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let mut strings = Strings::default();
        let mut st = Vec::new();
        write_node(&self.root, 0, &mut st, &mut strings)?;
        push_u32(&mut st, FDT_END);

        let off_rsv = HEADER_SIZE;
        let rsv_len = (self.reservations.len() + 1) * 16;
        let off_struct = off_rsv + rsv_len;
        let off_strings = off_struct + st.len();
        let total = off_strings + strings.bytes.len();
        let to32 = |v: usize| u32::try_from(v).map_err(|_| Error::TooLarge);

        let mut out = Vec::with_capacity(total);
        for w in [
            MAGIC,
            to32(total)?,
            to32(off_struct)?,
            to32(off_strings)?,
            to32(off_rsv)?,
            17,
            16,
            self.boot_cpuid_phys,
            to32(strings.bytes.len())?,
            to32(st.len())?,
        ] {
            push_u32(&mut out, w);
        }
        for (addr, size) in &self.reservations {
            out.extend_from_slice(&addr.to_be_bytes());
            out.extend_from_slice(&size.to_be_bytes());
        }
        out.extend_from_slice(&[0; 16]);
        out.extend_from_slice(&st);
        out.extend_from_slice(&strings.bytes);
        Ok(out)
    }
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn pad4(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

#[derive(Default)]
struct Strings {
    bytes: Vec<u8>,
    index: HashMap<String, u32>,
}

impl Strings {
    fn intern(&mut self, s: &str) -> Result<u32, Error> {
        if s.is_empty() || s.contains('\0') {
            return Err(Error::BadString);
        }
        if let Some(&off) = self.index.get(s) {
            return Ok(off);
        }
        let off = u32::try_from(self.bytes.len()).map_err(|_| Error::TooLarge)?;
        self.bytes.extend_from_slice(s.as_bytes());
        self.bytes.push(0);
        self.index.insert(s.into(), off);
        Ok(off)
    }
}

fn write_node(
    node: &NodeBuf,
    depth: usize,
    out: &mut Vec<u8>,
    strings: &mut Strings,
) -> Result<(), Error> {
    if depth >= MAX_DEPTH {
        return Err(Error::TooDeep);
    }
    // The root is always written with an empty name.
    let name = if depth == 0 { "" } else { node.name.as_str() };
    if name.contains('\0') || name.contains('/') || (depth > 0 && name.is_empty()) {
        return Err(Error::BadString);
    }
    push_u32(out, FDT_BEGIN_NODE);
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    pad4(out);
    for p in &node.props {
        push_u32(out, FDT_PROP);
        push_u32(
            out,
            u32::try_from(p.value.len()).map_err(|_| Error::TooLarge)?,
        );
        push_u32(out, strings.intern(&p.name)?);
        out.extend_from_slice(&p.value);
        pad4(out);
    }
    for c in &node.children {
        write_node(c, depth + 1, out, strings)?;
    }
    push_u32(out, FDT_END_NODE);
    Ok(())
}

/// A `/reserved-memory` child to add: `<name>@<hex addr>+<hex size>[,no-map]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionSpec {
    pub name: String,
    pub base: u64,
    pub size: u64,
    pub no_map: bool,
}

fn parse_hex(s: &str) -> Result<u64, Error> {
    let digits = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::BadRegion);
    }
    u64::from_str_radix(digits, 16).map_err(|_| Error::BadRegion)
}

impl RegionSpec {
    pub fn parse(spec: &str) -> Result<Self, Error> {
        let mut parts = spec.split(',');
        let head = parts.next().ok_or(Error::BadRegion)?;
        let mut no_map = false;
        for flag in parts {
            if flag != "no-map" {
                return Err(Error::BadRegion);
            }
            no_map = true;
        }
        let (name, rest) = head.split_once('@').ok_or(Error::BadRegion)?;
        let (addr, size) = rest.split_once('+').ok_or(Error::BadRegion)?;
        if name.is_empty() || name.contains(['/', '\0']) {
            return Err(Error::BadRegion);
        }
        let base = parse_hex(addr)?;
        let size = parse_hex(size)?;
        if size == 0 || base.checked_add(size).is_none() {
            return Err(Error::BadRegion);
        }
        Ok(RegionSpec {
            name: name.into(),
            base,
            size,
            no_map,
        })
    }

    /// The node name: `<name>@<lowercase hex address>`.
    pub fn node_name(&self) -> String {
        format!("{}@{:x}", self.name, self.base)
    }
}

impl Tree {
    /// Adds `regions` as children of `/reserved-memory`. Creates that node when missing, with `#address-cells` and
    /// `#size-cells` copied from the root (defaults 2 and 1 when the root has none) and an empty `ranges`. Fails
    /// with [`Error::Exists`] when a child of the same name is already there.
    pub fn add_reserved_regions(&mut self, regions: &[RegionSpec]) -> Result<(), Error> {
        let root_addr = self.root.prop_u32("#address-cells")?.unwrap_or(2);
        let root_size = self.root.prop_u32("#size-cells")?.unwrap_or(1);
        if self.find("/reserved-memory")?.is_none() {
            let rm = self.add_node("/reserved-memory")?;
            rm.set_prop("#address-cells", cells32(&[root_addr]));
            rm.set_prop("#size-cells", cells32(&[root_size]));
            rm.set_prop("ranges", Vec::new());
        }
        let rm = self.find_mut("/reserved-memory")?.ok_or(Error::NotFound)?;
        let addr_cells = rm.prop_u32("#address-cells")?.unwrap_or(root_addr);
        let size_cells = rm.prop_u32("#size-cells")?.unwrap_or(root_size);
        for r in regions {
            let name = r.node_name();
            if rm.child(&name).is_some() {
                return Err(Error::Exists);
            }
            let mut reg = cells(r.base, addr_cells)?;
            reg.extend(cells(r.size, size_cells)?);
            let mut node = NodeBuf::new(&name);
            node.set_prop("reg", reg);
            if r.no_map {
                node.set_prop("no-map", Vec::new());
            }
            rm.children.push(node);
        }
        Ok(())
    }
}

/// Pixel formats of the `simple-framebuffer` binding supported here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FbFormat {
    X8r8g8b8,
    A8r8g8b8,
    R5g6b5,
}

impl FbFormat {
    pub fn parse(s: &str) -> Result<Self, Error> {
        match s {
            "x8r8g8b8" => Ok(FbFormat::X8r8g8b8),
            "a8r8g8b8" => Ok(FbFormat::A8r8g8b8),
            "r5g6b5" => Ok(FbFormat::R5g6b5),
            _ => Err(Error::BadFramebuffer),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            FbFormat::X8r8g8b8 => "x8r8g8b8",
            FbFormat::A8r8g8b8 => "a8r8g8b8",
            FbFormat::R5g6b5 => "r5g6b5",
        }
    }

    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            FbFormat::X8r8g8b8 | FbFormat::A8r8g8b8 => 4,
            FbFormat::R5g6b5 => 2,
        }
    }
}

/// A `/chosen/framebuffer@<addr>` node (devicetree binding display/simple-framebuffer.yaml).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramebufferSpec {
    pub addr: u64,
    pub width: u32,
    pub height: u32,
    pub format: FbFormat,
    /// Bytes per line, when lines are padded; `None` for exactly `width` pixels.
    pub stride: Option<u32>,
    /// Also reserve the region as a `no-map` child of `/reserved-memory`.
    pub no_map: bool,
}

impl Tree {
    /// Adds `/chosen/framebuffer@<addr>`; with `no_map` also a `/reserved-memory` child of the same name and `reg`.
    /// `/chosen` and the root `#address-cells`/`#size-cells` must already exist. Nothing changes on error.
    pub fn add_framebuffer(&mut self, fb: &FramebufferSpec) -> Result<(), Error> {
        if fb.width == 0 || fb.height == 0 {
            return Err(Error::BadFramebuffer);
        }
        let line = fb
            .width
            .checked_mul(fb.format.bytes_per_pixel())
            .ok_or(Error::BadFramebuffer)?;
        let stride = fb.stride.unwrap_or(line);
        if stride < line {
            return Err(Error::BadFramebuffer);
        }
        let size = u64::from(stride) * u64::from(fb.height);
        if fb.addr.checked_add(size).is_none() {
            return Err(Error::BadFramebuffer);
        }
        let addr_cells = self
            .root
            .prop_u32("#address-cells")?
            .ok_or(Error::NotFound)?;
        let size_cells = self.root.prop_u32("#size-cells")?.ok_or(Error::NotFound)?;
        let mut reg = cells(fb.addr, addr_cells)?;
        reg.extend(cells(size, size_cells)?);

        let mut next = self.clone();
        let name = format!("framebuffer@{:x}", fb.addr);
        let chosen = next.find_mut("/chosen")?.ok_or(Error::NotFound)?;
        if chosen.child(&name).is_some() {
            return Err(Error::Exists);
        }
        // The `reg` of a child is read with the parent's cell sizes, which default to 2 and 1 when
        // missing. Linux device trees give `/chosen` the root's cells and an empty `ranges` when it has
        // framebuffer children; do the same unless `/chosen` already has its own.
        if chosen.prop("#address-cells").is_none() && chosen.prop("#size-cells").is_none() {
            chosen.set_prop("#address-cells", cells32(&[addr_cells]));
            chosen.set_prop("#size-cells", cells32(&[size_cells]));
            chosen.set_prop("ranges", Vec::new());
        } else if chosen.prop_u32("#address-cells")? != Some(addr_cells)
            || chosen.prop_u32("#size-cells")? != Some(size_cells)
        {
            return Err(Error::BadFramebuffer);
        }
        let mut node = NodeBuf::new(&name);
        node.set_prop("compatible", string("simple-framebuffer"));
        node.set_prop("reg", reg);
        node.set_prop("width", cells32(&[fb.width]));
        node.set_prop("height", cells32(&[fb.height]));
        node.set_prop("stride", cells32(&[stride]));
        node.set_prop("format", string(fb.format.as_str()));
        node.set_prop("status", string("okay"));
        chosen.children.push(node);
        if fb.no_map {
            next.add_reserved_regions(&[RegionSpec {
                name: "framebuffer".into(),
                base: fb.addr,
                size,
                no_map: true,
            }])?;
        }
        *self = next;
        Ok(())
    }
}
