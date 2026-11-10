//! The no_std, allocation-free rewriter: copies a validated blob into a caller's buffer with a few property edits.
//!
//! Built for LucinaBoot, which has no heap: it sets `/chosen` (`bootargs`, `linux,initrd-*`) and `/memory` in the
//! device tree it hands to the kernel. The output is a fresh version-17 blob: header, the memory reservation block
//! as it was, the structure block with the edits applied and every FDT_NOP dropped, then the strings block as it
//! was followed by any property names it lacked.

use crate::{
    Error, Fdt, HEADER_SIZE, MAGIC, MAX_DEPTH,
    reader::{FDT_BEGIN_NODE, FDT_END, FDT_END_NODE, FDT_PROP, Token},
};

/// Most edits one [`patch`] call takes.
pub const MAX_EDITS: usize = 24;

/// One change to a property.
///
/// `node` is an absolute path, looked up like [`Fdt::node`] (a component without a unit address also matches the
/// first `name@...` child). When several edits name the same property of the same node, the last one wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit<'e> {
    /// Sets the property: replaces its value when it exists, adds it after the node's other properties otherwise.
    /// When the node does not exist but its parent does, the node is created as the parent's last child.
    Set {
        node: &'e str,
        name: &'e str,
        value: &'e [u8],
    },
    /// Removes the property if it exists; a missing node or property is not an error.
    Remove { node: &'e str, name: &'e str },
}

impl<'e> Edit<'e> {
    fn node(&self) -> &'e str {
        match *self {
            Edit::Set { node, .. } | Edit::Remove { node, .. } => node,
        }
    }

    fn name(&self) -> &'e str {
        match *self {
            Edit::Set { name, .. } | Edit::Remove { name, .. } => name,
        }
    }
}

/// Where an edit applies, by structure-block offset of the node's body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target<'e> {
    Existing {
        body: usize,
        has_prop: bool,
    },
    New {
        parent: usize,
        name: &'e str,
    },
    /// A `Remove` on a node that does not exist.
    Nowhere,
}

/// Writes `fdt` with `edits` applied into `out` and returns the length of the new blob.
///
/// Errors: [`Error::NotFound`] when an edited node and its parent are both missing, [`Error::BadPath`] for a
/// malformed path or a name with `/` or NUL in it, [`Error::TooLarge`] for more than [`MAX_EDITS`] edits or a blob
/// beyond the format's 32-bit offsets, [`Error::BufferTooSmall`] when `out` cannot hold the result. `out` may be
/// partly written on error.
pub fn patch(fdt: &Fdt<'_>, edits: &[Edit<'_>], out: &mut [u8]) -> Result<usize, Error> {
    if edits.len() > MAX_EDITS {
        return Err(Error::TooLarge);
    }
    let mut targets = [Target::Nowhere; MAX_EDITS];
    for (t, e) in targets.iter_mut().zip(edits) {
        *t = resolve(fdt, e)?;
    }
    let targets = &targets[..edits.len()];
    let p = Patcher {
        fdt,
        edits,
        targets,
    };

    let rsv = fdt.rsv;
    let off_rsv = HEADER_SIZE;
    let off_struct = off_rsv + rsv.len();
    let mut w = Writer {
        out,
        pos: off_struct,
    };
    w.check(off_rsv, rsv.len())?;
    w.out[off_rsv..off_struct].copy_from_slice(rsv);
    p.write_structure(&mut w)?;
    let off_strings = w.pos;
    w.bytes(fdt.strings)?;
    for (i, e) in edits.iter().enumerate() {
        if p.needs_new_name(i) {
            w.bytes(e.name().as_bytes())?;
            w.bytes(&[0])?;
        }
    }
    let total = w.pos;
    let total32 = u32::try_from(total).map_err(|_| Error::TooLarge)?;
    let words = [
        MAGIC,
        total32,
        off_struct as u32,
        off_strings as u32,
        off_rsv as u32,
        17,
        16,
        fdt.boot_cpuid_phys(),
        (total - off_strings) as u32,
        (off_strings - off_struct) as u32,
    ];
    for (i, word) in words.iter().enumerate() {
        w.out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    Ok(total)
}

fn resolve<'e>(fdt: &Fdt<'_>, e: &Edit<'e>) -> Result<Target<'e>, Error> {
    let path = e.node();
    if !path.starts_with('/') || e.name().is_empty() || e.name().contains(['/', '\0']) {
        return Err(Error::BadPath);
    }
    if let Some(node) = fdt.node(path)? {
        let has_prop = node.property(e.name())?.is_some();
        return Ok(Target::Existing {
            body: node.body,
            has_prop,
        });
    }
    if matches!(e, Edit::Remove { .. }) {
        return Ok(Target::Nowhere);
    }
    let trimmed = path.trim_end_matches('/');
    let (parent, name) = trimmed.rsplit_once('/').ok_or(Error::BadPath)?;
    if name.is_empty() || name.contains('\0') {
        return Err(Error::BadPath);
    }
    let parent = if parent.is_empty() { "/" } else { parent };
    let parent = fdt.node(parent)?.ok_or(Error::NotFound)?;
    Ok(Target::New {
        parent: parent.body,
        name,
    })
}

struct Writer<'o> {
    out: &'o mut [u8],
    pos: usize,
}

impl Writer<'_> {
    fn check(&self, at: usize, len: usize) -> Result<(), Error> {
        match at.checked_add(len) {
            Some(end) if end <= self.out.len() => Ok(()),
            _ => Err(Error::BufferTooSmall),
        }
    }

    fn bytes(&mut self, b: &[u8]) -> Result<(), Error> {
        self.check(self.pos, b.len())?;
        self.out[self.pos..self.pos + b.len()].copy_from_slice(b);
        self.pos += b.len();
        Ok(())
    }

    fn word(&mut self, v: u32) -> Result<(), Error> {
        self.bytes(&v.to_be_bytes())
    }

    fn pad4(&mut self) -> Result<(), Error> {
        while !self.pos.is_multiple_of(4) {
            self.bytes(&[0])?;
        }
        Ok(())
    }

    fn begin_node(&mut self, name: &str) -> Result<(), Error> {
        self.word(FDT_BEGIN_NODE)?;
        self.bytes(name.as_bytes())?;
        self.bytes(&[0])?;
        self.pad4()
    }

    fn prop(&mut self, name_off: usize, value: &[u8]) -> Result<(), Error> {
        let len = u32::try_from(value.len()).map_err(|_| Error::TooLarge)?;
        let name_off = u32::try_from(name_off).map_err(|_| Error::TooLarge)?;
        self.word(FDT_PROP)?;
        self.word(len)?;
        self.word(name_off)?;
        self.bytes(value)?;
        self.pad4()
    }
}

struct Patcher<'p, 'a, 'e> {
    fdt: &'p Fdt<'a>,
    edits: &'p [Edit<'e>],
    targets: &'p [Target<'e>],
}

impl Patcher<'_, '_, '_> {
    /// True when edit `i` is the last one for its (node, property) pair, so it is the one that counts.
    fn effective(&self, i: usize) -> bool {
        let (t, name) = (self.targets[i], self.edits[i].name());
        !(i + 1..self.edits.len()).any(|j| self.targets[j] == t && self.edits[j].name() == name)
    }

    /// The edit that decides property `name` of the existing node with body `body`, if any.
    fn edit_for(&self, body: usize, name: &str) -> Option<&Edit<'_>> {
        (0..self.edits.len())
            .rev()
            .find_map(|i| match self.targets[i] {
                Target::Existing { body: b, .. } if b == body && self.edits[i].name() == name => {
                    Some(&self.edits[i])
                }
                _ => None,
            })
    }

    /// Offset of `name` in the strings block, or `None` when it must be added.
    fn existing_name(&self, name: &str) -> Option<usize> {
        let strings = self.fdt.strings;
        let needle = name.as_bytes();
        (0..strings.len()).find(|&off| {
            (off == 0 || strings[off - 1] == 0)
                && strings.get(off..off + needle.len()) == Some(needle)
                && strings.get(off + needle.len()) == Some(&0)
        })
    }

    /// True when edit `i` adds a property name that neither the strings block nor an earlier edit provides.
    fn needs_new_name(&self, i: usize) -> bool {
        let name = self.edits[i].name();
        matches!(self.edits[i], Edit::Set { .. })
            && self.existing_name(name).is_none()
            && !(0..i)
                .any(|j| matches!(self.edits[j], Edit::Set { .. }) && self.edits[j].name() == name)
    }

    /// Strings-block offset for edit `i`'s property name in the output.
    fn name_off(&self, i: usize) -> usize {
        let name = self.edits[i].name();
        if let Some(off) = self.existing_name(name) {
            return off;
        }
        let mut off = self.fdt.strings.len();
        for j in 0..self.edits.len() {
            if self.needs_new_name(j) {
                if self.edits[j].name() == name {
                    return off;
                }
                off += self.edits[j].name().len() + 1;
            }
        }
        off
    }

    /// Properties added to the existing node `body` (edits whose property it lacks), after its own.
    fn add_props(&self, w: &mut Writer<'_>, body: usize) -> Result<(), Error> {
        for (i, e) in self.edits.iter().enumerate() {
            if let (
                Edit::Set { value, .. },
                Target::Existing {
                    body: b,
                    has_prop: false,
                },
            ) = (e, self.targets[i])
                && b == body
                && self.effective(i)
            {
                w.prop(self.name_off(i), value)?;
            }
        }
        Ok(())
    }

    /// Nodes created under the existing node `body`, each holding every edit that targets it.
    fn new_children(&self, w: &mut Writer<'_>, body: usize) -> Result<(), Error> {
        for (i, t) in self.targets.iter().enumerate() {
            let Target::New { parent, name } = *t else {
                continue;
            };
            let first = !self.targets[..i].contains(t);
            if parent != body || !first {
                continue;
            }
            w.begin_node(name)?;
            for (j, e) in self.edits.iter().enumerate() {
                if let Edit::Set { value, .. } = e
                    && self.targets[j] == *t
                    && self.effective(j)
                {
                    w.prop(self.name_off(j), value)?;
                }
            }
            w.word(FDT_END_NODE)?;
        }
        Ok(())
    }

    fn write_structure(&self, w: &mut Writer<'_>) -> Result<(), Error> {
        // Per open node: its body offset and whether its added properties are written yet.
        let mut stack = [(0usize, false); MAX_DEPTH];
        let mut depth = 0usize;
        let mut off = 0usize;
        loop {
            let (tok, next) = self.fdt.token_at(off)?;
            match tok {
                Token::BeginNode(name) => {
                    if depth > 0 && !stack[depth - 1].1 {
                        self.add_props(w, stack[depth - 1].0)?;
                        stack[depth - 1].1 = true;
                    }
                    if depth == MAX_DEPTH {
                        return Err(Error::TooDeep);
                    }
                    stack[depth] = (next, false);
                    depth += 1;
                    w.begin_node(name)?;
                }
                Token::Prop(prop) => {
                    let body = stack[depth.checked_sub(1).ok_or(Error::BadStructure)?].0;
                    match self.edit_for(body, prop.name) {
                        Some(Edit::Remove { .. }) => {}
                        Some(Edit::Set { value, .. }) => {
                            // A duplicate property gets the new value once; later copies are dropped.
                            if !self.seen_before(body, prop.name, off)? {
                                let name_off = self.original_name_off(prop.name);
                                w.prop(name_off, value)?;
                            }
                        }
                        None => w.prop(self.original_name_off(prop.name), prop.value)?,
                    }
                }
                Token::Nop => {}
                Token::EndNode => {
                    depth = depth.checked_sub(1).ok_or(Error::BadStructure)?;
                    let (body, done) = stack[depth];
                    if !done {
                        self.add_props(w, body)?;
                    }
                    self.new_children(w, body)?;
                    w.word(FDT_END_NODE)?;
                }
                Token::End => {
                    w.word(FDT_END)?;
                    return Ok(());
                }
            }
            off = next;
        }
    }

    /// True when the node with body `body` has a property `name` before structure offset `at`.
    fn seen_before(&self, body: usize, name: &str, at: usize) -> Result<bool, Error> {
        let mut off = body;
        while off < at {
            let (tok, next) = self.fdt.token_at(off)?;
            match tok {
                Token::Prop(p) if p.name == name => return Ok(true),
                Token::Prop(_) | Token::Nop => off = next,
                _ => return Ok(false),
            }
        }
        Ok(false)
    }

    /// Where the reader found `name`: a slice of the strings block, so its offset is a pointer difference.
    fn original_name_off(&self, name: &str) -> usize {
        name.as_ptr() as usize - self.fdt.strings.as_ptr() as usize
    }
}
