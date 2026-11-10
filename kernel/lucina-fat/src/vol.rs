//! A mounted FAT volume: the FAT itself, cluster chains, directories, files and the metadata that ties them
//! together.
//!
//! The kernel keeps a [`Node`] per open file or directory and passes it to the operations here. Everything is
//! synchronous and single-threaded; the kernel serialises calls with one lock per volume.

use alloc::{vec, vec::Vec};

use crate::{
    bpb::{FatType, Geometry},
    dirent::{
        ATTR_ARCH, ATTR_DIR, DirEntry, MAX_DIR_SLOTS, SLOT, ShortFields, lfn_slots, scan,
        short_bytes,
    },
    error::{Error, Result},
    io::{BlockIo, CACHE_BLOCK, CLASS_BOOT, CLASS_DATA, CLASS_DIR, CLASS_FAT, Cache},
    name::{decode_short, long_units, names_equal, plan_short},
    time::{self, Time},
};

/// Source of the current time for new and changed entries.
pub type Clock = fn() -> Time;

/// Identity of a directory: 0 for the fixed root directory of FAT12/16, otherwise its first cluster.
pub type DirId = u32;

/// Where a short entry lives: the directory and the slot index in it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Pos {
    pub dir: DirId,
    pub slot: u32,
}

/// The in-memory state of one file or directory.
#[derive(Clone, Debug)]
pub struct Node {
    pub is_dir: bool,
    /// First cluster of the data, 0 for an empty file. FAT12/16 root: 0. FAT32 root: the root cluster.
    pub first_cluster: u32,
    /// File size in bytes; for a directory the size of its cluster chain.
    pub size: u32,
    pub attr: u8,
    pub lcase: u8,
    pub short: [u8; 11],
    pub ctime: Time,
    pub mtime: Time,
    pub atime: Time,
    /// Where the entry lives. `None` for the root and for a node whose entry has been removed.
    pub pos: Option<Pos>,
    /// The directory the node lives in.
    pub parent: DirId,
    /// Whether the entry on disk is out of date; see [`Volume::sync_node`].
    pub dirty: bool,
    /// (chain index, cluster) of a cluster known to be in the chain, to speed up sequential access.
    hint: (u32, u32),
}

impl Node {
    /// The identity of this directory.
    pub fn dir_id(&self) -> DirId {
        self.first_cluster
    }
}

/// File system statistics.
#[derive(Clone, Copy, Debug)]
pub struct StatFs {
    pub cluster_size: u32,
    pub clusters: u32,
    pub free_clusters: u32,
    pub name_max: u32,
    pub volume_id: u32,
}

/// Mount options.
#[derive(Clone, Copy, Debug)]
pub struct MountOptions {
    pub readonly: bool,
    /// Seconds east of UTC that the timestamps on disk are in (Linux `time_offset`, in seconds).
    pub time_offset: i64,
    /// Capacity of the block cache in 4096-byte blocks.
    pub cache_blocks: usize,
    pub clock: Clock,
    /// Set the dirty bit in the boot sector while the volume is in use, as Linux does. Off (`nodirtybit`), the block
    /// holding the boot sector is not written at all on a volume whose FSInfo lies in another block: it is the one
    /// block firmware needs that has no copy it would use.
    pub mark_dirty: bool,
    /// Keep the modification time when a write changes neither the size nor the clusters (`nomtime`): an overwrite in
    /// place then writes only its data blocks and never the directory block that holds the file's neighbours.
    pub keep_mtime: bool,
}

fn no_clock() -> Time {
    Time::default()
}

impl Default for MountOptions {
    fn default() -> Self {
        MountOptions {
            readonly: false,
            time_offset: 0,
            cache_blocks: 2048,
            clock: no_clock,
            mark_dirty: true,
            keep_mtime: false,
        }
    }
}

struct DirData {
    id: DirId,
    clusters: Vec<u32>,
    data: Vec<u8>,
    /// Range of slots changed in `data` and not yet written to the cache.
    lo: usize,
    hi: usize,
}

impl DirData {
    fn slots(&self) -> usize {
        self.data.len() / SLOT
    }

    fn touch(&mut self, from: usize, to: usize) {
        self.lo = self.lo.min(from);
        self.hi = self.hi.max(to);
    }
}

/// A mounted volume.
pub struct Volume<D: BlockIo> {
    cache: Cache<D>,
    g: Geometry,
    opts: MountOptions,
    free_clusters: Option<u32>,
    next_free: u32,
    fsinfo_valid: bool,
    /// The free count was counted from the FAT, and differing FAT copies were made equal. Done once, before the first
    /// change of the FAT: a hint or a second copy left by a crash or another system is not to be built on.
    fat_checked: bool,
    /// FSInfo on the device says "free count unknown" because the FAT is changing; see [`Volume::before_fat_change`].
    fsinfo_unknown: bool,
    /// The volume was marked dirty when it was mounted (an unclean shutdown); never cleared by us.
    was_dirty: bool,
    /// We set the dirty bit and have to clear it again.
    marked_dirty: bool,
    /// Something changed since the last [`Volume::sync`].
    modified: bool,
    root_time: Time,
    root_size: u32,
}

fn fat_decode_unit(t: FatType, b: &[u8], idx: usize) -> u32 {
    match t {
        FatType::Fat16 => u32::from(u16::from_le_bytes([b[idx * 2], b[idx * 2 + 1]])),
        FatType::Fat32 => {
            u32::from_le_bytes([b[idx * 4], b[idx * 4 + 1], b[idx * 4 + 2], b[idx * 4 + 3]])
                & 0x0FFF_FFFF
        }
        FatType::Fat12 => unreachable!("FAT12 is decoded entry by entry"),
    }
}

impl<D: BlockIo> Volume<D> {
    /// Mounts the volume on `dev`. Nothing is written.
    pub fn mount(dev: D, opts: MountOptions) -> Result<Self> {
        let size = dev.size();
        let mut cache = Cache::new(dev, opts.cache_blocks);
        if size < 512 {
            return Err(Error::Unsupported);
        }
        let mut boot = vec![0u8; 512];
        cache.read_at(0, &mut boot)?;
        let sector = usize::from(u16::from_le_bytes([boot[11], boot[12]]));
        if matches!(sector, 1024 | 2048 | 4096) && size >= sector as u64 {
            boot.resize(sector, 0);
            cache.read_at(0, &mut boot)?;
        }
        let g = Geometry::parse(&boot, size)?;
        let was_dirty = g.state_at_mount & 1 != 0;
        let mut free_clusters = None;
        let mut next_free = 2;
        let mut fsinfo_valid = false;
        if g.fat_type == FatType::Fat32 && g.fsinfo_sector != 0 {
            let bps = g.bytes_per_sector as usize;
            let off = u64::from(g.fsinfo_sector) * bps as u64;
            if off + bps as u64 <= size && off != 0 {
                let mut b = vec![0u8; bps];
                cache.read_at(off, &mut b)?;
                let le = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
                let tail = 0xAA55_0000;
                if le(0) == 0x4161_5252
                    && le(484) == 0x6141_7272
                    && (le(508) == tail || le(bps - 4) == tail)
                {
                    fsinfo_valid = true;
                    let free = le(488);
                    // After an unclean shutdown the hint says nothing.
                    if free <= g.clusters && !was_dirty {
                        free_clusters = Some(free);
                    }
                    let next = le(492);
                    if (2..=g.max_cluster).contains(&next) {
                        next_free = next;
                    }
                }
            }
        }
        let root_time = (opts.clock)();
        let mut vol = Volume {
            cache,
            g,
            opts,
            free_clusters,
            next_free,
            fsinfo_valid,
            fat_checked: false,
            fsinfo_unknown: false,
            was_dirty,
            marked_dirty: false,
            modified: false,
            root_time,
            root_size: 0,
        };
        vol.root_size = if vol.fat32() {
            let chain = vol.chain(vol.g.root_cluster)?;
            (chain.len() as u64 * u64::from(vol.g.cluster_size)).min(u64::from(u32::MAX)) as u32
        } else {
            vol.g.root_bytes
        };
        Ok(vol)
    }

    pub fn geometry(&self) -> &Geometry {
        &self.g
    }

    pub fn is_readonly(&self) -> bool {
        self.opts.readonly
    }

    /// Whether the volume was mounted with its dirty bit set (not cleanly unmounted before).
    pub fn was_dirty(&self) -> bool {
        self.was_dirty
    }

    /// The root directory.
    pub fn root(&self) -> Node {
        Node {
            is_dir: true,
            first_cluster: self.g.root_cluster,
            size: self.root_size,
            attr: ATTR_DIR,
            lcase: 0,
            short: [b' '; 11],
            ctime: self.root_time,
            mtime: self.root_time,
            atime: self.root_time,
            pos: None,
            parent: self.g.root_cluster,
            dirty: false,
            hint: (0, self.g.root_cluster),
        }
    }

    /// The identity of the root directory.
    pub fn root_id(&self) -> DirId {
        self.g.root_cluster
    }

    /// Gives the device back. Call [`Volume::sync`] first.
    pub fn into_device(self) -> D {
        self.cache.into_device()
    }

    pub fn device(&self) -> &D {
        self.cache.device()
    }

    fn now(&self) -> Time {
        (self.opts.clock)()
    }

    fn fat32(&self) -> bool {
        self.g.fat_type == FatType::Fat32
    }

    // ---- the FAT ----

    fn fat_offset(&self, copy: u32, n: u32) -> u64 {
        let base = self.g.fat_start + u64::from(copy) * self.g.fat_bytes;
        base + match self.g.fat_type {
            FatType::Fat12 => u64::from(n) + u64::from(n) / 2,
            FatType::Fat16 => u64::from(n) * 2,
            FatType::Fat32 => u64::from(n) * 4,
        }
    }

    fn fat_get(&mut self, n: u32) -> Result<u32> {
        if n > self.g.max_cluster {
            return Err(Error::Corrupt);
        }
        let off = self.fat_offset(self.g.active_fat, n);
        match self.g.fat_type {
            FatType::Fat12 => {
                let mut b = [0u8; 2];
                self.cache.read_at(off, &mut b)?;
                let w = u32::from(u16::from_le_bytes(b));
                Ok(if n & 1 == 0 { w & 0xFFF } else { w >> 4 })
            }
            FatType::Fat16 => {
                let mut b = [0u8; 2];
                self.cache.read_at(off, &mut b)?;
                Ok(u32::from(u16::from_le_bytes(b)))
            }
            FatType::Fat32 => {
                let mut b = [0u8; 4];
                self.cache.read_at(off, &mut b)?;
                Ok(u32::from_le_bytes(b) & 0x0FFF_FFFF)
            }
        }
    }

    /// Sets FAT entry `n` in every copy of the FAT.
    fn fat_set(&mut self, n: u32, v: u32) -> Result<()> {
        if n > self.g.max_cluster {
            return Err(Error::Corrupt);
        }
        for copy in 0..self.g.num_fats {
            let off = self.fat_offset(copy, n);
            match self.g.fat_type {
                FatType::Fat12 => {
                    let mut b = [0u8; 2];
                    self.cache.read_at(off, &mut b)?;
                    let w = u16::from_le_bytes(b);
                    let v = (v & 0xFFF) as u16;
                    let w = if n & 1 == 0 {
                        (w & 0xF000) | v
                    } else {
                        (w & 0x000F) | (v << 4)
                    };
                    self.cache.write_at(off, &w.to_le_bytes(), CLASS_FAT)?;
                }
                FatType::Fat16 => {
                    self.cache
                        .write_at(off, &(v as u16).to_le_bytes(), CLASS_FAT)?;
                }
                FatType::Fat32 => {
                    let mut b = [0u8; 4];
                    self.cache.read_at(off, &mut b)?;
                    let old = u32::from_le_bytes(b);
                    let new = (old & 0xF000_0000) | (v & 0x0FFF_FFFF);
                    self.cache.write_at(off, &new.to_le_bytes(), CLASS_FAT)?;
                }
            }
        }
        Ok(())
    }

    /// The cluster after `c` in its chain, `None` at the end of the chain.
    fn fat_next(&mut self, c: u32) -> Result<Option<u32>> {
        if c < 2 || c > self.g.max_cluster {
            return Err(Error::Corrupt);
        }
        let v = self.fat_get(c)?;
        if self.g.is_eoc(v) {
            return Ok(None);
        }
        if v < 2 || v > self.g.max_cluster || self.g.is_bad(v) {
            return Err(Error::Corrupt);
        }
        Ok(Some(v))
    }

    /// All clusters of the chain starting at `start`.
    fn chain(&mut self, start: u32) -> Result<Vec<u32>> {
        let mut v = Vec::new();
        let mut c = start;
        loop {
            v.push(c);
            if v.len() > self.g.clusters as usize {
                return Err(Error::Corrupt);
            }
            match self.fat_next(c)? {
                Some(n) => c = n,
                None => return Ok(v),
            }
        }
    }

    /// Called before the FAT changes. The first time, the free count is taken from the FAT and differing FAT copies are
    /// made equal from the live one (a power cut between copies leaves them different; firmware and Linux read the
    /// first). Then FSInfo's free count goes to "unknown" on the device before any FAT entry can get there, and stays
    /// so until [`Volume::sync`] has everything on disk: firmware believes a count it finds valid (edk2 `FatGrowEof`),
    /// and after a cut no count would be right.
    fn before_fat_change(&mut self) -> Result<()> {
        if !self.fat_checked {
            self.make_fat_copies_equal()?;
            self.free_clusters = Some(self.count_free()?);
            self.fat_checked = true;
        }
        if self.fsinfo_valid && !self.fsinfo_unknown {
            let off = u64::from(self.g.fsinfo_sector) * u64::from(self.g.bytes_per_sector) + 488;
            self.cache
                .write_at(off, &u32::MAX.to_le_bytes(), CLASS_BOOT)?;
            self.cache.flush()?;
            self.fsinfo_unknown = true;
        }
        Ok(())
    }

    fn make_fat_copies_equal(&mut self) -> Result<()> {
        if !self.g.mirrored {
            return Ok(());
        }
        let live = self.fat_offset(self.g.active_fat, 0);
        let (mut a, mut b) = ([0u8; CACHE_BLOCK], [0u8; CACHE_BLOCK]);
        for copy in 0..self.g.num_fats {
            if copy == self.g.active_fat {
                continue;
            }
            let other = self.fat_offset(copy, 0);
            let mut pos = 0u64;
            while pos < self.g.fat_bytes {
                let n = (self.g.fat_bytes - pos).min(CACHE_BLOCK as u64) as usize;
                self.cache.read_at(live + pos, &mut a[..n])?;
                self.cache.read_at(other + pos, &mut b[..n])?;
                if a[..n] != b[..n] {
                    self.cache.write_at(other + pos, &a[..n], CLASS_FAT)?;
                }
                pos += n as u64;
            }
        }
        Ok(())
    }

    /// Allocates one cluster, links it behind `prev` and marks it end of chain.
    fn alloc_cluster(&mut self, prev: Option<u32>) -> Result<u32> {
        self.before_fat_change()?;
        if self.free_clusters == Some(0) {
            return Err(Error::NoSpace);
        }
        let n = self.g.clusters;
        let start = self.next_free.clamp(2, self.g.max_cluster);
        for i in 0..n {
            let c = 2 + (start - 2 + i) % n;
            if self.fat_get(c)? == 0 {
                self.fat_set(c, self.g.eoc())?;
                if let Some(p) = prev {
                    self.fat_set(p, c)?;
                }
                self.next_free = if c < self.g.max_cluster { c + 1 } else { 2 };
                if let Some(f) = self.free_clusters.as_mut() {
                    *f = f.saturating_sub(1);
                }
                return Ok(c);
            }
        }
        self.free_clusters = Some(0);
        Err(Error::NoSpace)
    }

    /// Frees the chain starting at `start`.
    fn free_chain(&mut self, start: u32) -> Result<()> {
        if start < 2 || start > self.g.max_cluster {
            return Err(Error::Corrupt);
        }
        self.before_fat_change()?;
        let mut c = start;
        let mut count = 0u32;
        loop {
            let next = self.fat_next(c);
            self.fat_set(c, 0)?;
            if let Some(f) = self.free_clusters.as_mut() {
                *f = (*f + 1).min(self.g.clusters);
            }
            count += 1;
            if count > self.g.clusters {
                return Err(Error::Corrupt);
            }
            match next {
                Ok(Some(n)) => c = n,
                Ok(None) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }

    fn count_free(&mut self) -> Result<u32> {
        let mut free = 0u32;
        if self.g.fat_type == FatType::Fat12 {
            for c in 2..=self.g.max_cluster {
                if self.fat_get(c)? == 0 {
                    free += 1;
                }
            }
            return Ok(free);
        }
        let width = (self.g.fat_type.bits() / 8) as usize;
        let mut buf = vec![0u8; 64 * 1024];
        let mut c = 2u32;
        while c <= self.g.max_cluster {
            let n = ((self.g.max_cluster - c + 1) as usize).min(buf.len() / width);
            let off = self.fat_offset(self.g.active_fat, c);
            self.cache.read_at(off, &mut buf[..n * width])?;
            for i in 0..n {
                if fat_decode_unit(self.g.fat_type, &buf, i) == 0 {
                    free += 1;
                }
            }
            c += n as u32;
        }
        Ok(free)
    }

    // ---- write bookkeeping ----

    fn check_writable(&self) -> Result<()> {
        if self.opts.readonly {
            return Err(Error::ReadOnly);
        }
        Ok(())
    }

    /// Called before every change: refuses on a read-only volume and sets the dirty bit on the first change.
    fn begin_write(&mut self) -> Result<()> {
        self.check_writable()?;
        if self.opts.mark_dirty && !self.was_dirty && !self.marked_dirty {
            self.cache.write_at(
                self.g.state_offset,
                &[self.g.state_at_mount | 1],
                CLASS_BOOT,
            )?;
            self.cache.flush()?;
            self.marked_dirty = true;
        }
        self.modified = true;
        Ok(())
    }

    /// What fsync(2) needs: everything cached reaches the medium (data, then the FAT, then directories, with a device
    /// flush after each), and the volume stays marked as in use. Dirty [`Node`]s must have been written with
    /// [`Volume::sync_node`] first. Neither the boot sector nor FSInfo is touched. After a device error this fails, and
    /// keeps failing.
    pub fn checkpoint(&mut self) -> Result<()> {
        if self.opts.readonly {
            return Ok(());
        }
        self.cache.flush()
    }

    /// What sync(2) and unmounting need: a [`Volume::checkpoint`], then the volume is made clean: FSInfo gets the real
    /// free count and, if this code marked the volume dirty, the bit is cleared, each once what it describes is
    /// on disk.
    pub fn sync(&mut self) -> Result<()> {
        if self.opts.readonly {
            return Ok(());
        }
        self.cache.flush()?;
        if self.fsinfo_unknown {
            let off = u64::from(self.g.fsinfo_sector) * u64::from(self.g.bytes_per_sector) + 488;
            let mut b = [0u8; 8];
            b[..4].copy_from_slice(&self.free_clusters.unwrap_or(0xFFFF_FFFF).to_le_bytes());
            b[4..].copy_from_slice(&self.next_free.to_le_bytes());
            self.cache.write_at(off, &b, CLASS_BOOT)?;
            self.cache.flush()?;
            self.fsinfo_unknown = false;
        }
        if self.marked_dirty {
            self.cache
                .write_at(self.g.state_offset, &[self.g.state_at_mount], CLASS_BOOT)?;
            self.cache.flush()?;
            self.marked_dirty = false;
        }
        self.modified = false;
        Ok(())
    }

    /// Switches between read-only and read-write (a remount). Going read-only writes everything back first and forgets
    /// the cached blocks, so that what is read from then on comes from the device.
    pub fn set_readonly(&mut self, readonly: bool) -> Result<()> {
        if readonly {
            self.sync()?;
            self.cache.drop_clean();
        }
        self.opts.readonly = readonly;
        Ok(())
    }

    /// Makes the allocator start from the lowest cluster. The tests use it to get freed clusters reused at once.
    #[doc(hidden)]
    pub fn rewind_allocator(&mut self) {
        self.next_free = 2;
    }

    /// File system statistics. Counts the free clusters on the first call if FSInfo did not say.
    pub fn statfs(&mut self) -> Result<StatFs> {
        let free = match self.free_clusters {
            Some(f) => f,
            None => {
                let f = self.count_free()?;
                self.free_clusters = Some(f);
                f
            }
        };
        Ok(StatFs {
            cluster_size: self.g.cluster_size,
            clusters: self.g.clusters,
            free_clusters: free,
            name_max: 255,
            volume_id: self.g.volume_id,
        })
    }

    // ---- directories ----

    fn load_dir(&mut self, id: DirId) -> Result<DirData> {
        if id == 0 {
            if self.fat32() {
                return Err(Error::Corrupt);
            }
            let mut data = vec![0u8; self.g.root_bytes as usize];
            self.cache.read_at(self.g.root_start, &mut data)?;
            return Ok(DirData {
                id,
                clusters: Vec::new(),
                data,
                lo: usize::MAX,
                hi: 0,
            });
        }
        let clusters = self.chain(id)?;
        let cs = self.g.cluster_size as usize;
        if clusters.len() * cs > MAX_DIR_SLOTS * SLOT {
            return Err(Error::Corrupt);
        }
        let mut data = vec![0u8; clusters.len() * cs];
        for (i, &c) in clusters.iter().enumerate() {
            self.cache
                .read_at(self.g.cluster_offset(c), &mut data[i * cs..(i + 1) * cs])?;
        }
        Ok(DirData {
            id,
            clusters,
            data,
            lo: usize::MAX,
            hi: 0,
        })
    }

    /// Device offset of byte `byte` of the directory and how many bytes follow contiguously.
    fn dir_dev_off(&self, d: &DirData, byte: usize) -> (u64, usize) {
        if d.id == 0 {
            (self.g.root_start + byte as u64, d.data.len() - byte)
        } else {
            let cs = self.g.cluster_size as usize;
            (
                self.g.cluster_offset(d.clusters[byte / cs]) + (byte % cs) as u64,
                cs - byte % cs,
            )
        }
    }

    /// Writes the changed slots of `d` into the cache.
    fn commit_dir(&mut self, d: &mut DirData) -> Result<()> {
        if d.lo >= d.hi {
            return Ok(());
        }
        let (mut from, to) = (d.lo * SLOT, d.hi * SLOT);
        while from < to {
            let (off, room) = self.dir_dev_off(d, from);
            let n = room.min(to - from);
            self.cache
                .write_at(off, &d.data[from..from + n], CLASS_DIR)?;
            from += n;
        }
        d.lo = usize::MAX;
        d.hi = 0;
        Ok(())
    }

    fn dir_extend(&mut self, d: &mut DirData) -> Result<()> {
        let cs = self.g.cluster_size as usize;
        if d.id == 0 || d.data.len() + cs > MAX_DIR_SLOTS * SLOT {
            return Err(Error::NoSpace);
        }
        // The chain must never reach a cluster that still holds old data, which would read as entries: the empty
        // cluster and its end-of-chain mark are on the medium before the link is.
        let prev = d.clusters.last().copied();
        let c = self.alloc_cluster(None)?;
        self.cache.zero_at(
            self.g.cluster_offset(c),
            u64::from(self.g.cluster_size),
            CLASS_DATA,
        )?;
        self.cache.flush()?;
        if let Some(p) = prev {
            self.fat_set(p, c)?;
            self.cache.flush()?;
        }
        d.clusters.push(c);
        d.data.resize(d.data.len() + cs, 0);
        Ok(())
    }

    /// The device block that holds a slot.
    fn slot_block(&self, d: &DirData, slot: usize) -> u64 {
        self.dir_dev_off(d, slot * SLOT).0 / CACHE_BLOCK as u64
    }

    /// Whether the slots `[from, to)`, which all exist, lie in one device block, and so reach the medium in one write.
    fn in_one_block(&self, d: &DirData, from: usize, to: usize) -> bool {
        let first = self.slot_block(d, from);
        (from..to).all(|s| self.slot_block(d, s) == first)
    }

    /// Finds `n` consecutive free slots, extending the directory if needed. They lie in one device block when clusters
    /// allow that, so that a cut never leaves long-name slots without their short entry.
    fn find_slots(&mut self, d: &mut DirData, n: usize) -> Result<usize> {
        let total = d.slots();
        // With smaller clusters a long name can need more than one block.
        let whole = d.id == 0 || self.g.cluster_size as usize >= CACHE_BLOCK;
        let mut run: Option<usize> = None;
        let mut start = None;
        for i in 0..total {
            match d.data[i * SLOT] {
                0x00 => {
                    start = Some(run.unwrap_or(i));
                    break;
                }
                0xE5 => {
                    let mut s = *run.get_or_insert(i);
                    if whole && !self.in_one_block(d, s, i + 1) {
                        s = i;
                        run = Some(i);
                    }
                    if i + 1 - s >= n {
                        return Ok(s);
                    }
                }
                _ => run = None,
            }
        }
        let mut start = start.unwrap_or_else(|| run.unwrap_or(total));
        // The run ends in the free space at the end of the directory. If the entry would cross a block there, it starts
        // at the next block instead and the slots before it become deleted ones, so that the end mark does not hide it.
        if whole {
            let end = (start + n).min(total);
            if start < total && (start + n > total || !self.in_one_block(d, start, end)) {
                let next = (start..total)
                    .find(|&s| self.slot_block(d, s) != self.slot_block(d, start))
                    .unwrap_or(total);
                if d.id != 0 || next + n <= total {
                    for s in start..next {
                        d.data[s * SLOT] = 0xE5;
                    }
                    d.touch(start, next);
                    start = next;
                }
            }
        }
        if start + n > MAX_DIR_SLOTS {
            return Err(Error::NoSpace);
        }
        while d.slots() < start + n {
            self.dir_extend(d)?;
        }
        Ok(start)
    }

    /// Puts `bytes` (whole slots) at `start`. If that used the end mark, the slot behind must be one: firmware stops
    /// at the first zero byte, and anything left behind it by another system would appear.
    fn place_slots(d: &mut DirData, start: usize, bytes: &[u8]) {
        let n = bytes.len() / SLOT;
        let ate_end_mark = (start..start + n).any(|s| d.data[s * SLOT] == 0x00);
        d.data[start * SLOT..(start + n) * SLOT].copy_from_slice(bytes);
        d.touch(start, start + n);
        if ate_end_mark {
            let total = d.slots();
            let mut s = start + n;
            while s < total && d.data[s * SLOT] != 0x00 {
                d.data[s * SLOT..(s + 1) * SLOT].fill(0);
                d.touch(s, s + 1);
                s += 1;
            }
        }
    }

    fn entries(&self, d: &DirData) -> Vec<DirEntry> {
        let mut v = Vec::new();
        scan(&d.data, self.fat32(), self.opts.time_offset, &mut |e| {
            v.push(e);
            true
        });
        v
    }

    fn matches(e: &DirEntry, name: &str) -> bool {
        names_equal(&e.name, name)
            || (e.first_slot != e.short_slot && names_equal(&decode_short(&e.short, e.lcase), name))
    }

    fn node_from_entry(&mut self, dir: DirId, e: &DirEntry) -> Result<Node> {
        let is_dir = e.is_dir();
        let mut size = e.size;
        if is_dir {
            if e.first_cluster < 2 || e.first_cluster > self.g.max_cluster {
                return Err(Error::Corrupt);
            }
            let chain = self.chain(e.first_cluster)?;
            size = (chain.len() as u64 * u64::from(self.g.cluster_size)).min(u64::from(u32::MAX))
                as u32;
        }
        Ok(Node {
            is_dir,
            first_cluster: e.first_cluster,
            size,
            attr: e.attr,
            lcase: e.lcase,
            short: e.short,
            ctime: e.ctime,
            mtime: e.mtime,
            atime: e.atime,
            pos: Some(Pos {
                dir,
                slot: e.short_slot,
            }),
            parent: dir,
            dirty: false,
            hint: (0, e.first_cluster),
        })
    }

    /// Looks `name` up in `dir`.
    pub fn lookup(&mut self, dir: &Node, name: &str) -> Result<Node> {
        if !dir.is_dir {
            return Err(Error::NotDir);
        }
        let name = match long_units(name) {
            Ok((n, _)) => n,
            Err(Error::NameTooLong) => return Err(Error::NameTooLong),
            Err(_) => return Err(Error::NotFound),
        };
        let d = self.load_dir(dir.dir_id())?;
        let mut found = None;
        let (fat32, tz) = (self.fat32(), self.opts.time_offset);
        scan(&d.data, fat32, tz, &mut |e| {
            if Self::matches(&e, &name) {
                found = Some(e);
                false
            } else {
                true
            }
        });
        let e = found.ok_or(Error::NotFound)?;
        self.node_from_entry(d.id, &e)
    }

    /// Lists the entries of `dir` whose first slot is at or after `from_slot`, in on-disk order.
    pub fn read_dir(
        &mut self,
        dir: &Node,
        from_slot: u32,
        f: &mut dyn FnMut(&DirEntry) -> bool,
    ) -> Result<()> {
        if !dir.is_dir {
            return Err(Error::NotDir);
        }
        let d = self.load_dir(dir.dir_id())?;
        let (fat32, tz) = (self.fat32(), self.opts.time_offset);
        scan(&d.data, fat32, tz, &mut |e| {
            if e.first_slot < from_slot {
                return true;
            }
            f(&e)
        });
        Ok(())
    }

    /// Whether the directory has no entries (apart from `.` and `..`).
    pub fn dir_is_empty(&mut self, dir: &Node) -> Result<bool> {
        let d = self.load_dir(dir.dir_id())?;
        let mut empty = true;
        let (fat32, tz) = (self.fat32(), self.opts.time_offset);
        scan(&d.data, fat32, tz, &mut |_| {
            empty = false;
            false
        });
        Ok(empty)
    }

    /// Whether `dir` is a live directory: the root or one that still has its entry.
    fn is_live_dir(&self, dir: &Node) -> bool {
        dir.is_dir && (dir.pos.is_some() || dir.dir_id() == self.g.root_cluster)
    }

    fn parent_ref(&self, dir: DirId) -> u32 {
        if dir == self.g.root_cluster { 0 } else { dir }
    }

    /// Adds a (long +) short entry for `fields` to `d` and returns the slot of the short entry.
    fn add_entry(
        &mut self,
        d: &mut DirData,
        units: &[u16],
        needs_lfn: bool,
        fields: &ShortFields,
    ) -> Result<u32> {
        let mut bytes = if needs_lfn {
            lfn_slots(units, &fields.short)
        } else {
            Vec::new()
        };
        bytes.extend_from_slice(&short_bytes(fields, self.fat32(), self.opts.time_offset));
        let n = bytes.len() / SLOT;
        let start = self.find_slots(d, n)?;
        Self::place_slots(d, start, &bytes);
        Ok((start + n - 1) as u32)
    }

    /// Creates a file or directory called `name` in `dir`.
    pub fn create(&mut self, dir: &mut Node, name: &str, is_dir: bool) -> Result<Node> {
        if !dir.is_dir {
            return Err(Error::NotDir);
        }
        if !self.is_live_dir(dir) {
            return Err(Error::NotFound);
        }
        self.check_writable()?;
        let (name, units) = long_units(name)?;
        let mut d = self.load_dir(dir.dir_id())?;
        let existing = self.entries(&d);
        if existing.iter().any(|e| Self::matches(e, &name)) {
            return Err(Error::Exists);
        }
        let plan = plan_short(&name, &mut |raw| {
            let mut r = *raw;
            if r[0] == 0x05 {
                r[0] = 0xE5;
            }
            existing.iter().any(|e| e.short == r)
        })?;
        self.begin_write()?;
        let now = self.now();
        let mut cluster = 0;
        if is_dir {
            cluster = self.alloc_cluster(None)?;
            if let Err(e) = self.init_dir_cluster(cluster, self.parent_ref(dir.dir_id()), now) {
                let _ = self.free_chain(cluster);
                return Err(e);
            }
            // The new directory is complete on the medium before an entry can name it.
            self.cache.flush()?;
        }
        let attr = if is_dir { ATTR_DIR } else { ATTR_ARCH };
        let mut short = plan.raw;
        if short[0] == 0x05 {
            short[0] = 0xE5;
        }
        let fields = ShortFields {
            short: plan.raw,
            attr,
            lcase: 0,
            first_cluster: cluster,
            size: 0,
            ctime: now,
            mtime: now,
            atime: now,
        };
        let slot = match self.add_entry(&mut d, &units, plan.needs_lfn, &fields) {
            Ok(s) => s,
            Err(e) => {
                if cluster != 0 {
                    let _ = self.free_chain(cluster);
                }
                return Err(e);
            }
        };
        self.commit_dir(&mut d)?;
        dir.mtime = now;
        dir.dirty = true;
        dir.size = d.data.len() as u32;
        Ok(Node {
            is_dir,
            first_cluster: cluster,
            size: if is_dir { self.g.cluster_size } else { 0 },
            attr,
            lcase: 0,
            short,
            ctime: now,
            mtime: now,
            atime: now,
            pos: Some(Pos { dir: d.id, slot }),
            parent: d.id,
            dirty: false,
            hint: (0, cluster),
        })
    }

    fn init_dir_cluster(&mut self, cluster: u32, parent: u32, now: Time) -> Result<()> {
        let off = self.g.cluster_offset(cluster);
        self.cache
            .zero_at(off, u64::from(self.g.cluster_size), CLASS_DATA)?;
        let mk = |short: &[u8; 11], cl: u32| ShortFields {
            short: *short,
            attr: ATTR_DIR,
            lcase: 0,
            first_cluster: cl,
            size: 0,
            ctime: now,
            mtime: now,
            atime: now,
        };
        let (fat32, tz) = (self.fat32(), self.opts.time_offset);
        let dot = short_bytes(&mk(b".          ", cluster), fat32, tz);
        let dotdot = short_bytes(&mk(b"..         ", parent), fat32, tz);
        self.cache.write_at(off, &dot, CLASS_DIR)?;
        self.cache.write_at(off + SLOT as u64, &dotdot, CLASS_DIR)?;
        Ok(())
    }

    /// Finds the entry of `node` in `d`.
    fn entry_of(&self, d: &DirData, node: &Node) -> Result<DirEntry> {
        let pos = node.pos.ok_or(Error::NotFound)?;
        if pos.dir != d.id {
            return Err(Error::Invalid);
        }
        let mut found = None;
        scan(&d.data, self.fat32(), self.opts.time_offset, &mut |e| {
            if e.short_slot == pos.slot {
                found = Some(e);
                false
            } else {
                e.short_slot < pos.slot
            }
        });
        let e = found.ok_or(Error::Corrupt)?;
        if e.short != node.short {
            return Err(Error::Corrupt);
        }
        Ok(e)
    }

    fn mark_deleted(d: &mut DirData, e: &DirEntry) {
        for s in e.first_slot..=e.short_slot {
            d.data[s as usize * SLOT] = 0xE5;
        }
        d.touch(e.first_slot as usize, e.short_slot as usize + 1);
    }

    /// Removes the directory entry of `node` from `dir`; the node's clusters stay allocated until
    /// [`Volume::release`] is called on it. The node is detached (no entry any more).
    pub fn remove_entry(&mut self, dir: &mut Node, node: &mut Node) -> Result<()> {
        self.check_writable()?;
        let mut d = self.load_dir(dir.dir_id())?;
        let e = self.entry_of(&d, node)?;
        if node.is_dir && !self.dir_is_empty(node)? {
            return Err(Error::NotEmpty);
        }
        self.begin_write()?;
        Self::mark_deleted(&mut d, &e);
        self.commit_dir(&mut d)?;
        node.pos = None;
        node.dirty = false;
        let now = self.now();
        dir.mtime = now;
        dir.dirty = true;
        Ok(())
    }

    /// Frees the clusters of a node whose entry is gone and that nobody uses any more. The removal of the entry reaches
    /// the medium first: until it has, the entry on disk still names these clusters, so they must stay allocated in the
    /// FAT on disk and must not be given to another file, whose data would overwrite them. A cut after this leaves at
    /// worst lost clusters.
    pub fn release(&mut self, node: &mut Node) -> Result<()> {
        if node.first_cluster == 0 || node.pos.is_some() {
            return Ok(());
        }
        self.begin_write()?;
        self.cache.flush()?;
        let first = node.first_cluster;
        node.first_cluster = 0;
        node.size = 0;
        node.hint = (0, 0);
        self.free_chain(first)
    }

    /// Writes the entry of `node` if it changed.
    pub fn sync_node(&mut self, node: &mut Node) -> Result<()> {
        if !node.dirty {
            return Ok(());
        }
        let Some(pos) = node.pos else {
            node.dirty = false;
            return Ok(());
        };
        if self.opts.readonly {
            // In-memory changes of a read-only mount (utimensat, say) are not kept.
            node.dirty = false;
            return Ok(());
        }
        self.begin_write()?;
        let off = self.slot_offset(pos)?;
        let mut b = [0u8; SLOT];
        self.cache.read_at(off, &mut b)?;
        let mut short = node.short;
        if short[0] == 0xE5 {
            short[0] = 0x05;
        }
        let mut on_disk = [0u8; 11];
        on_disk.copy_from_slice(&b[..11]);
        if on_disk != short && on_disk != node.short {
            return Err(Error::Corrupt);
        }
        let tz = self.opts.time_offset;
        b[11] = node.attr;
        let (md, mt, _) = time::to_dos(node.mtime, tz);
        let (ad, _, _) = time::to_dos(node.atime, tz);
        b[18..20].copy_from_slice(&ad.to_le_bytes());
        if self.fat32() {
            b[20..22].copy_from_slice(&((node.first_cluster >> 16) as u16).to_le_bytes());
        }
        b[22..24].copy_from_slice(&mt.to_le_bytes());
        b[24..26].copy_from_slice(&md.to_le_bytes());
        b[26..28].copy_from_slice(&(node.first_cluster as u16).to_le_bytes());
        let size = if node.is_dir { 0 } else { node.size };
        b[28..32].copy_from_slice(&size.to_le_bytes());
        self.cache.write_at(off, &b, CLASS_DIR)?;
        node.dirty = false;
        Ok(())
    }

    /// Device offset of a slot.
    fn slot_offset(&mut self, pos: Pos) -> Result<u64> {
        let byte = u64::from(pos.slot) * SLOT as u64;
        if pos.dir == 0 {
            if self.fat32() || byte + SLOT as u64 > u64::from(self.g.root_bytes) {
                return Err(Error::Corrupt);
            }
            return Ok(self.g.root_start + byte);
        }
        let cs = u64::from(self.g.cluster_size);
        let mut c = pos.dir;
        for _ in 0..byte / cs {
            c = self.fat_next(c)?.ok_or(Error::Corrupt)?;
        }
        Ok(self.g.cluster_offset(c) + byte % cs)
    }

    /// Renames and/or moves `node` (currently in `old_dir`) to `new_name` in `new_dir` (`None`: the same
    /// directory). `replace` is the node that currently has the target name, if any; its entry is taken over and its
    /// clusters stay allocated until [`Volume::release`].
    ///
    /// **Over an existing target** the target's name is always there, naming the old or the new file, never neither: the
    /// target's short entry takes the new file's fields in place, keeping its name (as Linux `vfat_rename` does). This
    /// is durable when the call returns. See [`Self::rename_over`].
    ///
    /// **With no target**, within one device block the entry moves in one write, so a cut leaves the old name or the new
    /// one. Otherwise the new name is added first, so for a moment both names lead to the same clusters (fsck.fat
    /// reports that), and the file is never lost. Durable after the next [`Volume::checkpoint`].
    pub fn rename(
        &mut self,
        old_dir: &mut Node,
        node: &mut Node,
        new_dir: Option<&mut Node>,
        new_name: &str,
        replace: Option<&mut Node>,
    ) -> Result<()> {
        self.check_writable()?;
        let (name, units) = long_units(new_name)?;
        let old_pos = node.pos.ok_or(Error::NotFound)?;
        if old_pos.dir != old_dir.dir_id() {
            return Err(Error::Invalid);
        }
        let same_dir = new_dir.is_none();
        let new_id = new_dir.as_ref().map_or(old_dir.dir_id(), |d| d.dir_id());
        if new_dir.as_ref().is_some_and(|nd| !nd.is_dir) {
            return Err(Error::NotDir);
        }
        if new_dir.as_ref().is_some_and(|nd| !self.is_live_dir(nd)) {
            return Err(Error::NotFound);
        }
        if node.is_dir && !same_dir {
            self.check_not_inside(new_id, node.first_cluster)?;
        }
        let mut d_old = self.load_dir(old_dir.dir_id())?;
        let old_entry = self.entry_of(&d_old, node)?;
        let mut d_new_storage = if same_dir {
            None
        } else {
            Some(self.load_dir(new_id)?)
        };
        let now = self.now();
        let existing = self.entries(d_new_storage.as_ref().unwrap_or(&d_old));
        let mut replaced = None;
        if let Some(t) = existing.iter().find(|e| Self::matches(e, &name)) {
            let is_self = same_dir && t.short_slot == old_pos.slot;
            if is_self && old_entry.name == name {
                // The same name in the same directory: nothing to do.
                return Ok(());
            }
            if !is_self {
                let r = replace.ok_or(Error::Exists)?;
                if r.pos
                    != Some(Pos {
                        dir: new_id,
                        slot: t.short_slot,
                    })
                {
                    return Err(Error::Exists);
                }
                if node.is_dir != t.is_dir() {
                    return Err(if t.is_dir() {
                        Error::IsDir
                    } else {
                        Error::NotDir
                    });
                }
                if t.is_dir() && !self.dir_is_empty_node(r)? {
                    return Err(Error::NotEmpty);
                }
                replaced = Some((t.clone(), r));
            }
        }
        self.begin_write()?;
        if let Some((t, r)) = replaced {
            self.rename_over(&mut d_old, d_new_storage.as_mut(), &old_entry, &t, node)?;
            r.pos = None;
            r.dirty = false;
        } else {
            self.rename_plain(
                &mut d_old,
                d_new_storage.as_mut(),
                &old_entry,
                &existing,
                (&name, &units),
                node,
            )?;
        }
        old_dir.mtime = now;
        old_dir.dirty = true;
        if same_dir {
            old_dir.size = (d_old.data.len() as u64).min(u64::from(u32::MAX)) as u32;
        }
        if let Some(nd) = new_dir {
            nd.mtime = now;
            nd.dirty = true;
            if let Some(d) = d_new_storage.as_ref()
                && nd.dir_id() != 0
            {
                nd.size = (d.data.len() as u64).min(u64::from(u32::MAX)) as u32;
            }
        }
        Ok(())
    }

    /// The fields of `node` as a short entry with the given name.
    fn node_fields(node: &Node, short: [u8; 11], lcase: u8) -> ShortFields {
        ShortFields {
            short,
            attr: node.attr,
            lcase,
            first_cluster: node.first_cluster,
            size: if node.is_dir { 0 } else { node.size },
            ctime: node.ctime,
            mtime: node.mtime,
            atime: node.atime,
        }
    }

    /// Takes over the entry `t` for `node`.
    ///
    /// 1. All that `node` holds reaches the medium (its data, then its FAT chain).
    /// 2. Its old entry goes. A cut here leaves the target as it was, and the new file's clusters lost.
    /// 3. The target's short entry, one block write, names the new clusters. A cut here leaves the new file under
    ///    the target's name, and the old clusters lost.
    ///
    /// When the old entry and the target's short entry share a block, 2 and 3 are one write. The old clusters are freed
    /// by [`Volume::release`], after all of this.
    fn rename_over(
        &mut self,
        d_old: &mut DirData,
        d_new: Option<&mut DirData>,
        old: &DirEntry,
        t: &DirEntry,
        node: &mut Node,
    ) -> Result<()> {
        self.cache.flush()?;
        if let (Some(dn), true) = (d_new.as_ref(), node.is_dir) {
            let parent = self.parent_ref(dn.id);
            self.set_dotdot(node.first_cluster, parent)?;
            self.cache.flush()?;
        }
        let slot = t.short_slot as usize;
        let new_id;
        match d_new {
            None => {
                new_id = d_old.id;
                let blk = self.slot_block(d_old, slot);
                let together = (old.first_slot as usize..=old.short_slot as usize)
                    .all(|s| self.slot_block(d_old, s) == blk);
                Self::mark_deleted(d_old, old);
                if !together {
                    self.commit_dir(d_old)?;
                    self.cache.flush()?;
                }
                self.put_over(d_old, slot, node);
                self.commit_dir(d_old)?;
            }
            Some(dn) => {
                new_id = dn.id;
                Self::mark_deleted(d_old, old);
                self.commit_dir(d_old)?;
                self.cache.flush()?;
                self.put_over(dn, slot, node);
                self.commit_dir(dn)?;
            }
        }
        self.cache.flush()?;
        node.short = t.short;
        node.lcase = t.lcase;
        node.pos = Some(Pos {
            dir: new_id,
            slot: t.short_slot,
        });
        node.parent = new_id;
        node.dirty = false;
        Ok(())
    }

    /// Overwrites the short entry at `slot` with the fields of `node`, keeping its name and case bits.
    fn put_over(&self, d: &mut DirData, slot: usize, node: &Node) {
        let raw = &d.data[slot * SLOT..(slot + 1) * SLOT];
        let mut name = [0u8; 11];
        name.copy_from_slice(&raw[..11]);
        let lcase = raw[12];
        let bytes = short_bytes(
            &Self::node_fields(node, name, lcase),
            self.fat32(),
            self.opts.time_offset,
        );
        d.data[slot * SLOT..(slot + 1) * SLOT].copy_from_slice(&bytes);
        d.touch(slot, slot + 1);
    }

    /// A rename with no target; see [`Volume::rename`].
    fn rename_plain(
        &mut self,
        d_old: &mut DirData,
        mut d_new: Option<&mut DirData>,
        old: &DirEntry,
        existing: &[DirEntry],
        name: (&str, &[u16]),
        node: &mut Node,
    ) -> Result<()> {
        let (name, units) = name;
        let short_taken = |skip: Option<u32>, raw: &[u8; 11]| {
            let mut r = *raw;
            if r[0] == 0x05 {
                r[0] = 0xE5;
            }
            existing
                .iter()
                .any(|e| Some(e.short_slot) != skip && e.short == r)
        };
        let finish = |node: &mut Node, raw: [u8; 11], id: DirId, slot: u32| {
            let mut short = raw;
            if short[0] == 0x05 {
                short[0] = 0xE5;
            }
            node.short = short;
            node.lcase = 0;
            node.pos = Some(Pos { dir: id, slot });
            node.parent = id;
            node.dirty = false;
        };
        // Within one block: free the old slots, and if the new entry fits in that block, one write does it all.
        if d_new.is_none()
            && self.in_one_block(d_old, old.first_slot as usize, old.short_slot as usize + 1)
        {
            let blk = self.slot_block(d_old, old.short_slot as usize);
            let plan = plan_short(name, &mut |raw| short_taken(Some(old.short_slot), raw))?;
            let mut bytes = if plan.needs_lfn {
                lfn_slots(units, &plan.raw)
            } else {
                Vec::new()
            };
            let fields = Self::node_fields(node, plan.raw, 0);
            bytes.extend_from_slice(&short_bytes(&fields, self.fat32(), self.opts.time_offset));
            let n = bytes.len() / SLOT;
            let saved = (d_old.data.clone(), d_old.lo, d_old.hi);
            Self::mark_deleted(d_old, old);
            if let Some(start) = self.free_run_in_block(d_old, blk, n) {
                Self::place_slots(d_old, start, &bytes);
                self.commit_dir(d_old)?;
                finish(node, plan.raw, d_old.id, (start + n - 1) as u32);
                return Ok(());
            }
            (d_old.data, d_old.lo, d_old.hi) = saved;
        }
        // The new name first.
        let plan = plan_short(name, &mut |raw| short_taken(None, raw))?;
        let fields = Self::node_fields(node, plan.raw, 0);
        let target: &mut DirData = match d_new.as_deref_mut() {
            Some(d) => d,
            None => &mut *d_old,
        };
        let slot = self.add_entry(target, units, plan.needs_lfn, &fields)?;
        self.commit_dir(target)?;
        let id = target.id;
        self.cache.flush()?;
        if d_new.is_some() && node.is_dir {
            // ".." now names the new parent.
            let parent = self.parent_ref(id);
            self.set_dotdot(node.first_cluster, parent)?;
        }
        Self::mark_deleted(d_old, old);
        self.commit_dir(d_old)?;
        finish(node, plan.raw, id, slot);
        Ok(())
    }

    /// `n` consecutive free slots inside device block `blk`, counting everything behind the end mark as free.
    fn free_run_in_block(&self, d: &DirData, blk: u64, n: usize) -> Option<usize> {
        let mut run = None;
        let mut past_end = false;
        for i in 0..d.slots() {
            let first = d.data[i * SLOT];
            past_end |= first == 0x00;
            if (past_end || first == 0xE5) && self.slot_block(d, i) == blk {
                let s = *run.get_or_insert(i);
                if i + 1 - s >= n {
                    return Some(s);
                }
            } else {
                run = None;
            }
        }
        None
    }

    /// Points the `..` entry of a directory at `parent`.
    fn set_dotdot(&mut self, dir_cluster: u32, parent: u32) -> Result<()> {
        let off = self.g.cluster_offset(dir_cluster) + SLOT as u64;
        let mut b = [0u8; SLOT];
        self.cache.read_at(off, &mut b)?;
        if &b[..11] != b"..         " {
            return Err(Error::Corrupt);
        }
        if self.fat32() {
            b[20..22].copy_from_slice(&((parent >> 16) as u16).to_le_bytes());
        }
        b[26..28].copy_from_slice(&(parent as u16).to_le_bytes());
        self.cache.write_at(off, &b, CLASS_DIR)
    }

    fn dir_is_empty_node(&mut self, n: &Node) -> Result<bool> {
        self.dir_is_empty(n)
    }

    /// Fails if directory `dir` is `ancestor` or lies below it.
    fn check_not_inside(&mut self, dir: DirId, ancestor: u32) -> Result<()> {
        let mut id = dir;
        for _ in 0..=self.g.clusters {
            if id == ancestor {
                return Err(Error::Invalid);
            }
            if id == self.g.root_cluster {
                return Ok(());
            }
            let mut b = [0u8; SLOT];
            self.cache
                .read_at(self.g.cluster_offset(id) + SLOT as u64, &mut b)?;
            let hi = if self.fat32() {
                u32::from(u16::from_le_bytes([b[20], b[21]])) << 16
            } else {
                0
            };
            let up = hi | u32::from(u16::from_le_bytes([b[26], b[27]]));
            id = if up == 0 { self.g.root_cluster } else { up };
        }
        Err(Error::Corrupt)
    }

    // ---- file data ----

    /// Cluster number of chain index `idx`, `None` if the chain is shorter.
    fn cluster_at(&mut self, n: &mut Node, idx: u32) -> Result<Option<u32>> {
        if n.first_cluster == 0 {
            return Ok(None);
        }
        let (mut i, mut c) = if n.hint.1 != 0 && n.hint.0 <= idx {
            n.hint
        } else {
            (0, n.first_cluster)
        };
        while i < idx {
            match self.fat_next(c)? {
                Some(x) => {
                    c = x;
                    i += 1;
                }
                None => return Ok(None),
            }
        }
        n.hint = (i, c);
        Ok(Some(c))
    }

    /// Like [`Self::cluster_at`] but extends the chain as far as needed.
    fn cluster_alloc_at(&mut self, n: &mut Node, idx: u32) -> Result<u32> {
        if let Some(c) = self.cluster_at(n, idx)? {
            return Ok(c);
        }
        // Find the tail of the chain.
        let mut tail: Option<(u32, u32)> = None;
        if n.first_cluster != 0 {
            let (mut i, mut c) = if n.hint.1 != 0 {
                n.hint
            } else {
                (0, n.first_cluster)
            };
            while let Some(x) = self.fat_next(c)? {
                c = x;
                i += 1;
            }
            tail = Some((i, c));
        }
        let (mut ti, mut tc) = match tail {
            Some(t) => t,
            None => {
                let c = self.alloc_cluster(None)?;
                n.first_cluster = c;
                n.dirty = true;
                (0, c)
            }
        };
        while ti < idx {
            let c = self.alloc_cluster(Some(tc))?;
            ti += 1;
            tc = c;
            n.hint = (ti, tc);
        }
        n.hint = (ti, tc);
        Ok(tc)
    }

    /// Reads file data. Returns the number of bytes read (0 at or past the end).
    pub fn read(&mut self, n: &mut Node, off: u64, buf: &mut [u8]) -> Result<usize> {
        if n.is_dir {
            return Err(Error::IsDir);
        }
        let size = u64::from(n.size);
        if off >= size || buf.is_empty() {
            return Ok(0);
        }
        let want = (buf.len() as u64).min(size - off) as usize;
        let cs = u64::from(self.g.cluster_size);
        let mut done = 0usize;
        while done < want {
            let pos = off + done as u64;
            let Some(c) = self.cluster_at(n, (pos / cs) as u32)? else {
                return if done > 0 {
                    Ok(done)
                } else {
                    Err(Error::Corrupt)
                };
            };
            let inner = pos % cs;
            let take = ((cs - inner) as usize).min(want - done);
            self.cache.read_at(
                self.g.cluster_offset(c) + inner,
                &mut buf[done..done + take],
            )?;
            done += take;
        }
        Ok(done)
    }

    /// Zeroes `[from, to)` of the file, allocating clusters as needed.
    fn zero_range(&mut self, n: &mut Node, from: u64, to: u64) -> Result<()> {
        let cs = u64::from(self.g.cluster_size);
        let mut pos = from;
        while pos < to {
            let inner = pos % cs;
            let take = (cs - inner).min(to - pos);
            let c = self.cluster_alloc_at(n, (pos / cs) as u32)?;
            self.cache
                .zero_at(self.g.cluster_offset(c) + inner, take, CLASS_DATA)?;
            pos += take;
        }
        Ok(())
    }

    /// Writes file data, extending the file (a gap is filled with zeros). Returns the number of bytes written,
    /// which is short if the volume is full or the file reaches 4 GiB - 1.
    pub fn write(&mut self, n: &mut Node, off: u64, buf: &[u8]) -> Result<usize> {
        self.begin_write()?;
        if n.is_dir {
            return Err(Error::IsDir);
        }
        if buf.is_empty() {
            return Ok(0);
        }
        let limit = u64::from(u32::MAX);
        if off >= limit {
            return Err(Error::TooBig);
        }
        let len = (buf.len() as u64).min(limit - off) as usize;
        let old = u64::from(n.size);
        if off > old {
            self.zero_range(n, old, off)?;
        }
        let cs = u64::from(self.g.cluster_size);
        let mut done = 0usize;
        while done < len {
            let pos = off + done as u64;
            let c = match self.cluster_alloc_at(n, (pos / cs) as u32) {
                Ok(c) => c,
                Err(Error::NoSpace) if done > 0 => break,
                Err(e) => return Err(e),
            };
            let inner = pos % cs;
            let take = ((cs - inner) as usize).min(len - done);
            self.cache.write_at(
                self.g.cluster_offset(c) + inner,
                &buf[done..done + take],
                CLASS_DATA,
            )?;
            done += take;
        }
        let end = off + done as u64;
        if end > old {
            n.size = end as u32;
        }
        // With `keep_mtime`, an overwrite that changed neither size nor clusters leaves the directory alone.
        if end > old || !self.opts.keep_mtime {
            n.mtime = self.now();
            n.dirty = true;
        }
        Ok(done)
    }

    /// Sets the size of a file; growing fills with zeros. When shrinking, the entry with the new size reaches the medium
    /// before the clusters are given up: a cut in between leaves a file shorter than its chain, with its data intact.
    pub fn truncate(&mut self, n: &mut Node, new_size: u64) -> Result<()> {
        self.begin_write()?;
        if n.is_dir {
            return Err(Error::IsDir);
        }
        if new_size > u64::from(u32::MAX) {
            return Err(Error::TooBig);
        }
        let old = u64::from(n.size);
        if new_size > old {
            self.zero_range(n, old, new_size)?;
        } else if new_size < old {
            let cs = u64::from(self.g.cluster_size);
            let keep = new_size.div_ceil(cs) as u32;
            let first = n.first_cluster;
            let last = if keep == 0 {
                None
            } else {
                self.cluster_at(n, keep - 1)?
            };
            let next = match last {
                Some(l) => self.fat_next(l)?,
                None => None,
            };
            if keep == 0 {
                n.first_cluster = 0;
                n.hint = (0, 0);
            }
            n.size = new_size as u32;
            n.mtime = self.now();
            n.dirty = true;
            self.sync_node(n)?;
            self.cache.flush()?;
            if keep == 0 {
                if first != 0 {
                    self.free_chain(first)?;
                }
            } else if let Some(l) = last {
                self.before_fat_change()?;
                self.fat_set(l, self.g.eoc())?;
                if let Some(x) = next {
                    self.free_chain(x)?;
                }
            }
        }
        n.size = new_size as u32;
        n.mtime = self.now();
        n.dirty = true;
        Ok(())
    }
    /// Sets the access and/or modification time of a node.
    pub fn touch(&mut self, n: &mut Node, atime: Option<Time>, mtime: Option<Time>) {
        if let Some(t) = atime {
            n.atime = t;
        }
        if let Some(t) = mtime {
            n.mtime = t;
        }
        n.dirty = true;
    }
}
