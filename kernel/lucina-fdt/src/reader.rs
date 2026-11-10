//! The no_std, allocation-free reader.

use core::{fmt, str};

use crate::{Error, HEADER_SIZE, MAGIC};

pub(crate) const FDT_BEGIN_NODE: u32 = 1;
pub(crate) const FDT_END_NODE: u32 = 2;
pub(crate) const FDT_PROP: u32 = 3;
pub(crate) const FDT_NOP: u32 = 4;
pub(crate) const FDT_END: u32 = 9;

fn be32(buf: &[u8], off: usize) -> Result<u32, Error> {
    let end = off.checked_add(4).ok_or(Error::Truncated)?;
    let b: [u8; 4] = buf
        .get(off..end)
        .ok_or(Error::Truncated)?
        .try_into()
        .map_err(|_| Error::Truncated)?;
    Ok(u32::from_be_bytes(b))
}

fn be64(buf: &[u8], off: usize) -> Result<u64, Error> {
    let end = off.checked_add(8).ok_or(Error::Truncated)?;
    let b: [u8; 8] = buf
        .get(off..end)
        .ok_or(Error::Truncated)?
        .try_into()
        .map_err(|_| Error::Truncated)?;
    Ok(u64::from_be_bytes(b))
}

fn align4(x: usize) -> Result<usize, Error> {
    x.checked_add(3).map(|v| v & !3).ok_or(Error::Truncated)
}

/// The NUL-terminated UTF-8 string at `off`, and the length of its text.
fn cstr(buf: &[u8], off: usize) -> Result<(&str, usize), Error> {
    let rest = buf.get(off..).ok_or(Error::BadString)?;
    let n = rest.iter().position(|&b| b == 0).ok_or(Error::BadString)?;
    let text = rest.get(..n).ok_or(Error::BadString)?;
    let s = str::from_utf8(text).map_err(|_| Error::BadString)?;
    Ok((s, n))
}

fn range(data: &[u8], off: usize, size: usize) -> Result<&[u8], Error> {
    let end = off.checked_add(size).ok_or(Error::OutOfBounds)?;
    data.get(off..end).ok_or(Error::OutOfBounds)
}

/// The header fields [`Fdt::new`] relies on, as checked by [`check_header`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) total: usize,
    pub(crate) off_struct: usize,
    pub(crate) size_struct: usize,
    pub(crate) off_strings: usize,
    pub(crate) size_strings: usize,
    pub(crate) off_rsv: usize,
}

// `check_header` must stay token-for-token identical (ignoring `proof` blocks and comments) to the function of
// the same name in verification/fdt/header.rs, where Verus proves that it accepts exactly the headers whose
// blocks lie inside both `totalsize` and the buffer, aligned as the specification requires, without overflow.
// The `proof_model_matches_code` test enforces this.

/// Checks the ten header words (`magic`, `totalsize`, `off_dt_struct`, `off_dt_strings`, `off_mem_rsvmap`,
/// `version`, `last_comp_version`, `boot_cpuid_phys`, `size_dt_strings`, `size_dt_struct`) of a blob in a buffer
/// of `buf_len` bytes. The reservation block's extent is found by walking it, so only its start is checked here.
#[allow(clippy::manual_is_multiple_of)]
pub(crate) fn check_header(buf_len: usize, w: [u32; 10]) -> Result<Header, Error> {
    if buf_len < 8 {
        return Err(Error::Truncated);
    }
    if w[0] != MAGIC {
        return Err(Error::BadMagic);
    }
    let total = w[1] as usize;
    if total < HEADER_SIZE || total > buf_len {
        return Err(Error::BadTotalSize);
    }
    if w[5] < 17 {
        return Err(Error::UnsupportedVersion(w[5]));
    }
    if w[6] > 16 {
        return Err(Error::IncompatibleVersion(w[6]));
    }
    let off_struct = w[2] as usize;
    let off_strings = w[3] as usize;
    let off_rsv = w[4] as usize;
    let size_strings = w[8] as usize;
    let size_struct = w[9] as usize;
    if off_struct % 4 != 0 || size_struct % 4 != 0 || off_rsv % 8 != 0 {
        return Err(Error::Misaligned);
    }
    if off_struct > total || size_struct > total - off_struct {
        return Err(Error::OutOfBounds);
    }
    if off_strings > total || size_strings > total - off_strings {
        return Err(Error::OutOfBounds);
    }
    if off_rsv >= total {
        return Err(Error::OutOfBounds);
    }
    Ok(Header {
        total,
        off_struct,
        size_struct,
        off_strings,
        size_strings,
        off_rsv,
    })
}

/// Big-endian cells (4 bytes each) folded into a u64. Callers pass at most 8 bytes.
fn cells_value(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b))
}

/// A validated device tree blob.
#[derive(Clone, Copy)]
pub struct Fdt<'a> {
    pub(crate) structure: &'a [u8],
    pub(crate) strings: &'a [u8],
    /// Memory reservation block including its all-zero terminator.
    pub(crate) rsv: &'a [u8],
    version: u32,
    boot_cpuid_phys: u32,
    total_size: usize,
}

impl fmt::Debug for Fdt<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fdt")
            .field("version", &self.version)
            .field("total_size", &self.total_size)
            .field("struct_size", &self.structure.len())
            .field("strings_size", &self.strings.len())
            .finish_non_exhaustive()
    }
}

impl<'a> Fdt<'a> {
    /// Validates the header and the whole structure block of `buf` (which may be longer than `totalsize`).
    pub fn new(buf: &'a [u8]) -> Result<Self, Error> {
        let mut w = [0u32; 10];
        for (i, word) in w.iter_mut().enumerate() {
            // Words past a short buffer stay 0; check_header rejects such a buffer before using them.
            *word = be32(buf, 4 * i).unwrap_or(0);
        }
        let h = check_header(buf.len(), w)?;
        let (total_size, off_rsv) = (h.total, h.off_rsv);
        let (off_struct, size_struct) = (h.off_struct, h.size_struct);
        let (off_strings, size_strings) = (h.off_strings, h.size_strings);
        let (version, boot_cpuid_phys) = (w[5], w[7]);
        let data = buf.get(..total_size).ok_or(Error::BadTotalSize)?;
        let structure = range(data, off_struct, size_struct)?;
        let strings = range(data, off_strings, size_strings)?;

        // The reservation block has no size field: walk to its terminator.
        let mut p = off_rsv;
        loop {
            let addr = be64(data, p).map_err(|_| Error::OutOfBounds)?;
            let size = be64(data, p + 8).map_err(|_| Error::OutOfBounds)?;
            p += 16;
            if addr == 0 && size == 0 {
                break;
            }
        }
        let rsv = range(data, off_rsv, p - off_rsv)?;

        let spans = [
            (0, HEADER_SIZE),
            (off_rsv, p),
            (off_struct, off_struct + size_struct),
            (off_strings, off_strings + size_strings),
        ];
        for (i, a) in spans.iter().enumerate() {
            for b in spans.iter().skip(i + 1) {
                if a.0 < a.1 && b.0 < b.1 && a.0 < b.1 && b.0 < a.1 {
                    return Err(Error::Overlap);
                }
            }
        }

        let fdt = Fdt {
            structure,
            strings,
            rsv,
            version,
            boot_cpuid_phys,
            total_size,
        };
        fdt.validate()?;
        Ok(fdt)
    }

    fn validate(&self) -> Result<(), Error> {
        let mut depth = 0usize;
        let mut seen_root = false;
        for token in self.tokens() {
            match token? {
                Token::BeginNode(name) => {
                    if depth == 0 {
                        if seen_root || !name.is_empty() {
                            return Err(Error::BadStructure);
                        }
                        seen_root = true;
                    }
                    depth += 1;
                }
                Token::EndNode => depth = depth.checked_sub(1).ok_or(Error::BadStructure)?,
                Token::Prop(_) => {
                    if depth == 0 {
                        return Err(Error::BadStructure);
                    }
                }
                Token::Nop => {}
                Token::End => {
                    return if depth == 0 && seen_root {
                        Ok(())
                    } else {
                        Err(Error::BadStructure)
                    };
                }
            }
        }
        Err(Error::Truncated)
    }

    /// Header `version` (17 or newer).
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Header `boot_cpuid_phys`.
    pub fn boot_cpuid_phys(&self) -> u32 {
        self.boot_cpuid_phys
    }

    /// Header `totalsize`.
    pub fn total_size(&self) -> usize {
        self.total_size
    }

    /// The memory reservation block (`/memreserve/` entries).
    pub fn mem_reservations(&self) -> MemReservations<'a> {
        MemReservations { rest: self.rsv }
    }

    /// Every structure token, up to and including FDT_END; stops after the first error.
    pub fn tokens(&self) -> Tokens<'a> {
        Tokens {
            fdt: *self,
            off: 0,
            done: false,
        }
    }

    pub(crate) fn token_at(&self, off: usize) -> Result<(Token<'a>, usize), Error> {
        let tag = be32(self.structure, off)?;
        let p = off + 4;
        match tag {
            FDT_BEGIN_NODE => {
                let (name, n) = cstr(self.structure, p)?;
                let next = align4(p + n + 1)?;
                if next > self.structure.len() {
                    return Err(Error::Truncated);
                }
                Ok((Token::BeginNode(name), next))
            }
            FDT_END_NODE => Ok((Token::EndNode, p)),
            FDT_PROP => {
                let len = be32(self.structure, p)? as usize;
                let name_off = be32(self.structure, p + 4)? as usize;
                let start = p + 8;
                let end = start.checked_add(len).ok_or(Error::Truncated)?;
                let value = self.structure.get(start..end).ok_or(Error::Truncated)?;
                let (name, _) = cstr(self.strings, name_off)?;
                let next = align4(end)?;
                if next > self.structure.len() {
                    return Err(Error::Truncated);
                }
                Ok((Token::Prop(Property { name, value }), next))
            }
            FDT_NOP => Ok((Token::Nop, p)),
            FDT_END => Ok((Token::End, p)),
            other => Err(Error::BadToken(other)),
        }
    }

    /// The root node.
    pub fn root(&self) -> Result<Node<'a>, Error> {
        let mut off = 0;
        loop {
            match self.token_at(off)? {
                (Token::Nop, next) => off = next,
                (Token::BeginNode(name), next) => {
                    return Ok(Node {
                        fdt: *self,
                        name,
                        body: next,
                    });
                }
                _ => return Err(Error::BadStructure),
            }
        }
    }

    /// Looks a node up by absolute path (`/`, `/chosen`, `/soc/uart@1000`). A component without a unit address
    /// also matches the first node of that name, as in libfdt, when no node matches exactly.
    pub fn node(&self, path: &str) -> Result<Option<Node<'a>>, Error> {
        let rest = path.strip_prefix('/').ok_or(Error::BadPath)?;
        let mut cur = self.root()?;
        for comp in rest.split('/').filter(|c| !c.is_empty()) {
            match cur.child(comp)? {
                Some(n) => cur = n,
                None => return Ok(None),
            }
        }
        Ok(Some(cur))
    }

    /// Property `name` of the node at `path`.
    pub fn property(&self, path: &str, name: &str) -> Result<Option<Property<'a>>, Error> {
        match self.node(path)? {
            Some(n) => n.property(name),
            None => Ok(None),
        }
    }

    /// All `reg` ranges of the root's memory nodes (`device_type = "memory"`, or named `memory[@...]`).
    pub fn memory(&self) -> Result<MemoryRegions<'a>, Error> {
        let root = self.root()?;
        Ok(MemoryRegions {
            children: root.children(),
            addr_cells: root.address_cells()?,
            size_cells: root.size_cells()?,
            cur: RegIter::empty(),
        })
    }

    /// The children of `/reserved-memory`, or `None` when the node does not exist. Their `reg` uses the cells of
    /// `/reserved-memory` itself, falling back to the root's.
    pub fn reserved_memory(&self) -> Result<Option<ReservedRegions<'a>>, Error> {
        let root = self.root()?;
        let Some(rm) = root.child("reserved-memory")? else {
            return Ok(None);
        };
        let addr_cells = match rm.property("#address-cells")? {
            Some(p) => p.as_u32()?,
            None => root.address_cells()?,
        };
        let size_cells = match rm.property("#size-cells")? {
            Some(p) => p.as_u32()?,
            None => root.size_cells()?,
        };
        Ok(Some(ReservedRegions {
            children: rm.children(),
            addr_cells,
            size_cells,
        }))
    }

    fn framebuffer_in(&self, chosen: Node<'a>) -> Result<Option<Framebuffer<'a>>, Error> {
        let root = self.root()?;
        for c in chosen.children() {
            let c = c?;
            if c.name() != "framebuffer" && !c.name().starts_with("framebuffer@") {
                continue;
            }
            let Some(compat) = c.property("compatible")? else {
                continue;
            };
            let mut simple = false;
            for s in compat.strings() {
                simple |= s? == "simple-framebuffer";
            }
            if !simple {
                continue;
            }
            let reg = c.reg(root.address_cells()?, root.size_cells()?)?.next();
            let (Some(reg), Some(w), Some(h), Some(st), Some(f)) = (
                reg,
                c.property("width")?,
                c.property("height")?,
                c.property("stride")?,
                c.property("format")?,
            ) else {
                continue;
            };
            return Ok(Some(Framebuffer {
                base: reg.base,
                size: reg.size,
                width: w.as_u32()?,
                height: h.as_u32()?,
                stride: st.as_u32()?,
                format: f.as_str()?,
                name: c.name(),
            }));
        }
        Ok(None)
    }

    /// The `/chosen` node's boot parameters, or `None` when it does not exist.
    pub fn chosen(&self) -> Result<Option<Chosen<'a>>, Error> {
        let Some(node) = self.node("/chosen")? else {
            return Ok(None);
        };
        let bootargs = node.property("bootargs")?.map(|p| p.as_str()).transpose()?;
        let stdout_path = node
            .property("stdout-path")?
            .map(|p| p.as_str())
            .transpose()?;
        let start = node.property("linux,initrd-start")?;
        let end = node.property("linux,initrd-end")?;
        let initrd = match (start, end) {
            (Some(s), Some(e)) => Some((s.as_uint()?, e.as_uint()?)),
            _ => None,
        };
        let framebuffer = self.framebuffer_in(node)?;
        Ok(Some(Chosen {
            bootargs,
            stdout_path,
            initrd,
            framebuffer,
        }))
    }
}

/// One entry of the memory reservation block.
pub struct MemReservations<'a> {
    rest: &'a [u8],
}

impl Iterator for MemReservations<'_> {
    /// `(address, size)`
    type Item = (u64, u64);

    fn next(&mut self) -> Option<Self::Item> {
        let addr = be64(self.rest, 0).ok()?;
        let size = be64(self.rest, 8).ok()?;
        if addr == 0 && size == 0 {
            self.rest = &[];
            return None;
        }
        self.rest = self.rest.get(16..)?;
        Some((addr, size))
    }
}

/// A structure block token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token<'a> {
    BeginNode(&'a str),
    EndNode,
    Prop(Property<'a>),
    Nop,
    End,
}

/// Iterator over structure tokens, see [`Fdt::tokens`].
pub struct Tokens<'a> {
    fdt: Fdt<'a>,
    off: usize,
    done: bool,
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Result<Token<'a>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.fdt.token_at(self.off) {
            Ok((tok, next)) => {
                if tok == Token::End {
                    self.done = true;
                }
                self.off = next;
                Some(Ok(tok))
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

/// A node: its name and, through the iterators, its properties and children.
#[derive(Debug, Clone, Copy)]
pub struct Node<'a> {
    fdt: Fdt<'a>,
    name: &'a str,
    /// Offset in the structure block of the first token after the node's BEGIN_NODE.
    pub(crate) body: usize,
}

impl<'a> Node<'a> {
    /// Node name including the unit address (`memory@40000000`); empty for the root.
    pub fn name(&self) -> &'a str {
        self.name
    }

    /// This node's properties (those before its first child).
    pub fn properties(&self) -> Properties<'a> {
        Properties {
            fdt: self.fdt,
            off: self.body,
            done: false,
        }
    }

    /// This node's direct children.
    pub fn children(&self) -> Children<'a> {
        Children {
            fdt: self.fdt,
            off: self.body,
            done: false,
        }
    }

    pub fn property(&self, name: &str) -> Result<Option<Property<'a>>, Error> {
        for p in self.properties() {
            let p = p?;
            if p.name == name {
                return Ok(Some(p));
            }
        }
        Ok(None)
    }

    /// A direct child by exact name, else the first whose name is `comp@<unit address>`.
    pub fn child(&self, comp: &str) -> Result<Option<Node<'a>>, Error> {
        let mut fallback = None;
        for c in self.children() {
            let c = c?;
            if c.name == comp {
                return Ok(Some(c));
            }
            if fallback.is_none()
                && !comp.contains('@')
                && c.name.split_once('@').is_some_and(|(base, _)| base == comp)
            {
                fallback = Some(c);
            }
        }
        Ok(fallback)
    }

    /// `#address-cells` for this node's children (default 2).
    pub fn address_cells(&self) -> Result<u32, Error> {
        Ok(match self.property("#address-cells")? {
            Some(p) => p.as_u32()?,
            None => 2,
        })
    }

    /// `#size-cells` for this node's children (default 1).
    pub fn size_cells(&self) -> Result<u32, Error> {
        Ok(match self.property("#size-cells")? {
            Some(p) => p.as_u32()?,
            None => 1,
        })
    }

    /// `reg` of this node, decoded with the cells of its parent.
    pub fn reg(&self, addr_cells: u32, size_cells: u32) -> Result<RegIter<'a>, Error> {
        match self.property("reg")? {
            Some(p) => p.reg(addr_cells, size_cells),
            None => Ok(RegIter::empty()),
        }
    }
}

/// Iterator over a node's properties.
pub struct Properties<'a> {
    fdt: Fdt<'a>,
    off: usize,
    done: bool,
}

impl<'a> Iterator for Properties<'a> {
    type Item = Result<Property<'a>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.done {
            match self.fdt.token_at(self.off) {
                Ok((Token::Prop(p), next)) => {
                    self.off = next;
                    return Some(Ok(p));
                }
                Ok((Token::Nop, next)) => self.off = next,
                Ok(_) => self.done = true,
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            }
        }
        None
    }
}

/// Iterator over a node's direct children.
pub struct Children<'a> {
    fdt: Fdt<'a>,
    off: usize,
    done: bool,
}

impl<'a> Children<'a> {
    /// Offset just after the END_NODE that closes the subtree whose body starts at `off`.
    fn skip_subtree(&self, mut off: usize) -> Result<usize, Error> {
        let mut depth = 1usize;
        loop {
            let (tok, next) = self.fdt.token_at(off)?;
            off = next;
            match tok {
                Token::BeginNode(_) => depth += 1,
                Token::EndNode => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(off);
                    }
                }
                Token::End => return Err(Error::BadStructure),
                Token::Prop(_) | Token::Nop => {}
            }
        }
    }
}

impl<'a> Iterator for Children<'a> {
    type Item = Result<Node<'a>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.done {
            match self.fdt.token_at(self.off) {
                Ok((Token::Prop(_) | Token::Nop, next)) => self.off = next,
                Ok((Token::BeginNode(name), body)) => {
                    return match self.skip_subtree(body) {
                        Ok(after) => {
                            self.off = after;
                            Some(Ok(Node {
                                fdt: self.fdt,
                                name,
                                body,
                            }))
                        }
                        Err(e) => {
                            self.done = true;
                            Some(Err(e))
                        }
                    };
                }
                Ok(_) => self.done = true,
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            }
        }
        None
    }
}

/// A property: a name and a raw value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Property<'a> {
    pub name: &'a str,
    pub value: &'a [u8],
}

impl<'a> Property<'a> {
    /// True for a property with no value (a boolean flag such as `no-map`).
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    pub fn as_u32(&self) -> Result<u32, Error> {
        if self.value.len() != 4 {
            return Err(Error::BadLength);
        }
        be32(self.value, 0)
    }

    pub fn as_u64(&self) -> Result<u64, Error> {
        if self.value.len() != 8 {
            return Err(Error::BadLength);
        }
        be64(self.value, 0)
    }

    /// One cell or two, as a number (for `linux,initrd-start`-style properties).
    pub fn as_uint(&self) -> Result<u64, Error> {
        match self.value.len() {
            4 | 8 => Ok(cells_value(self.value)),
            _ => Err(Error::BadLength),
        }
    }

    /// A single NUL-terminated string.
    pub fn as_str(&self) -> Result<&'a str, Error> {
        let (last, body) = self.value.split_last().ok_or(Error::BadString)?;
        if *last != 0 || body.contains(&0) {
            return Err(Error::BadString);
        }
        str::from_utf8(body).map_err(|_| Error::BadString)
    }

    /// A string list (`compatible`).
    pub fn strings(&self) -> StringList<'a> {
        StringList {
            rest: self.value,
            done: false,
        }
    }

    /// Decodes a `reg`-style value given the parent's `#address-cells` (1 or 2) and `#size-cells` (0 to 2).
    pub fn reg(&self, addr_cells: u32, size_cells: u32) -> Result<RegIter<'a>, Error> {
        if !(1..=2).contains(&addr_cells) || size_cells > 2 {
            return Err(Error::BadCells);
        }
        let addr_len = addr_cells as usize * 4;
        let stride = addr_len + size_cells as usize * 4;
        if !self.value.len().is_multiple_of(stride) {
            return Err(Error::BadLength);
        }
        Ok(RegIter {
            rest: self.value,
            addr_len,
            stride,
        })
    }
}

/// Iterator over the strings of a string-list property.
pub struct StringList<'a> {
    rest: &'a [u8],
    done: bool,
}

impl<'a> Iterator for StringList<'a> {
    type Item = Result<&'a str, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.rest.is_empty() {
            return None;
        }
        match cstr(self.rest, 0) {
            Ok((s, n)) => {
                self.rest = self.rest.get(n + 1..).unwrap_or(&[]);
                Some(Ok(s))
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

/// An address range from a `reg` property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub base: u64,
    pub size: u64,
}

impl Region {
    /// One past the last byte, or `None` if it does not fit a u64.
    pub fn end(&self) -> Option<u64> {
        self.base.checked_add(self.size)
    }
}

/// Iterator over `(address, size)` pairs of a `reg` property.
#[derive(Debug, Clone, Copy)]
pub struct RegIter<'a> {
    rest: &'a [u8],
    addr_len: usize,
    stride: usize,
}

impl RegIter<'_> {
    fn empty() -> Self {
        RegIter {
            rest: &[],
            addr_len: 4,
            stride: 4,
        }
    }
}

impl Iterator for RegIter<'_> {
    type Item = Region;

    fn next(&mut self) -> Option<Region> {
        let (entry, rest) = self.rest.split_at_checked(self.stride)?;
        self.rest = rest;
        let (addr, size) = entry.split_at_checked(self.addr_len)?;
        Some(Region {
            base: cells_value(addr),
            size: cells_value(size),
        })
    }
}

fn is_memory_node(node: &Node<'_>) -> Result<bool, Error> {
    if node.name == "memory" || node.name.starts_with("memory@") {
        return Ok(true);
    }
    Ok(match node.property("device_type")? {
        Some(p) => p.as_str()? == "memory",
        None => false,
    })
}

/// Iterator over the memory ranges of the root's memory nodes, see [`Fdt::memory`].
pub struct MemoryRegions<'a> {
    children: Children<'a>,
    addr_cells: u32,
    size_cells: u32,
    cur: RegIter<'a>,
}

impl Iterator for MemoryRegions<'_> {
    type Item = Result<Region, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(r) = self.cur.next() {
                return Some(Ok(r));
            }
            let node = match self.children.next()? {
                Ok(n) => n,
                Err(e) => return Some(Err(e)),
            };
            match is_memory_node(&node) {
                Ok(true) => match node.reg(self.addr_cells, self.size_cells) {
                    Ok(it) => self.cur = it,
                    Err(e) => return Some(Err(e)),
                },
                Ok(false) => {}
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

/// A child of `/reserved-memory`.
#[derive(Debug, Clone, Copy)]
pub struct ReservedRegion<'a> {
    pub node: Node<'a>,
    pub no_map: bool,
    pub reusable: bool,
    addr_cells: u32,
    size_cells: u32,
}

impl<'a> ReservedRegion<'a> {
    pub fn name(&self) -> &'a str {
        self.node.name()
    }

    /// The `reg` ranges (none for a dynamically placed region that only has `size`).
    pub fn regs(&self) -> Result<RegIter<'a>, Error> {
        self.node.reg(self.addr_cells, self.size_cells)
    }
}

/// Iterator over the children of `/reserved-memory`, see [`Fdt::reserved_memory`].
pub struct ReservedRegions<'a> {
    children: Children<'a>,
    addr_cells: u32,
    size_cells: u32,
}

impl<'a> Iterator for ReservedRegions<'a> {
    type Item = Result<ReservedRegion<'a>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let node = match self.children.next()? {
            Ok(n) => n,
            Err(e) => return Some(Err(e)),
        };
        let flags = (|| {
            Ok((
                node.property("no-map")?.is_some(),
                node.property("reusable")?.is_some(),
            ))
        })();
        Some(flags.map(|(no_map, reusable)| ReservedRegion {
            node,
            no_map,
            reusable,
            addr_cells: self.addr_cells,
            size_cells: self.size_cells,
        }))
    }
}

/// A `simple-framebuffer` node (display/simple-framebuffer.yaml).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Framebuffer<'a> {
    pub base: u64,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: &'a str,
    pub name: &'a str,
}

/// Boot parameters from `/chosen`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chosen<'a> {
    pub bootargs: Option<&'a str>,
    pub stdout_path: Option<&'a str>,
    /// `(linux,initrd-start, linux,initrd-end)`
    pub initrd: Option<(u64, u64)>,
    /// A `simple-framebuffer` child (`framebuffer` or `framebuffer@<addr>`).
    pub framebuffer: Option<Framebuffer<'a>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Body of `fn name(` in `src`, with comments, `proof { .. }` blocks and whitespace removed. The body is the
    /// first block opened by `opener` after the name (Verus puts it on its own line, after `ensures`).
    fn body(src: &str, name: &str, opener: &str) -> String {
        let start = src
            .find(&format!("fn {name}("))
            .unwrap_or_else(|| panic!("fn {name} not found"));
        let open = start + src[start..].find(opener).unwrap() + opener.find('{').unwrap();
        let mut text: String = src[open + 1..matching(src, open)]
            .lines()
            .map(|l| l.split("//").next().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        while let Some(i) = text.find("proof {") {
            let end = matching(&text, i + "proof ".len());
            text.replace_range(i..=end, "");
        }
        text.chars().filter(|c| !c.is_whitespace()).collect()
    }

    /// Index of the `}` matching the `{` at `open`.
    fn matching(s: &str, open: usize) -> usize {
        let mut depth = 0;
        for (i, c) in s[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return open + i;
                    }
                }
                _ => {}
            }
        }
        panic!("unbalanced braces");
    }

    #[test]
    fn proof_model_matches_code() {
        const MODEL: &str = include_str!("../../../verification/fdt/header.rs");
        const CODE: &str = include_str!("reader.rs");
        let model = body(MODEL, "check_header", "\n{\n");
        let code = body(CODE, "check_header", " {\n");
        assert!(code.contains("returnErr(Error::Truncated)"));
        assert_eq!(
            model, code,
            "check_header in reader.rs differs from the Verus model"
        );
    }

    #[test]
    fn check_header_cases() {
        let good = [MAGIC, 64, 40, 60, 48, 17, 16, 0, 4, 8];
        let h = check_header(64, good).unwrap();
        assert_eq!(
            (h.total, h.off_struct, h.size_struct, h.off_rsv),
            (64, 40, 8, 48)
        );
        assert_eq!(check_header(7, good), Err(Error::Truncated));
        assert_eq!(check_header(63, good), Err(Error::BadTotalSize));
        let with = |i: usize, v: u32| {
            let mut w = good;
            w[i] = v;
            check_header(64, w)
        };
        assert_eq!(with(0, 0), Err(Error::BadMagic));
        assert_eq!(with(1, 39), Err(Error::BadTotalSize));
        assert_eq!(with(5, 16), Err(Error::UnsupportedVersion(16)));
        assert_eq!(with(6, 17), Err(Error::IncompatibleVersion(17)));
        assert_eq!(with(2, 42), Err(Error::Misaligned));
        assert_eq!(with(9, 6), Err(Error::Misaligned));
        assert_eq!(with(4, 44), Err(Error::Misaligned));
        assert_eq!(with(9, 28), Err(Error::OutOfBounds));
        assert_eq!(with(3, 61), Err(Error::OutOfBounds));
        assert_eq!(with(8, u32::MAX), Err(Error::OutOfBounds));
        assert_eq!(with(4, 64), Err(Error::OutOfBounds));
    }
}
