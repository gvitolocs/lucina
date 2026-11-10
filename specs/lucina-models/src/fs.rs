//! File-system durability: spec `docs/spec/fs-durability.md`, layers (c) and (d).
//!
//! [`Replace`]: one program replaces a file the safe way (write a new file, fsync it, rename it over the old name, fsync the
//! folder) while the kernel's writeback may write any dirty block at any moment and the device's volatile cache may make any
//! written block durable in any order. A power cut can come at any moment: every state is checked against every disk image a
//! cut could leave (each block either its durable value or the one written to the cache, never torn: assumption D1).
//!
//! [`SyncAll`]: sync(2) over several mounted file systems, one of whose flushes fails (patch 1116).

use stateright::{Model, Property};

/// The folder's one block: which file the name `target` points to (1 the old, 2 the new), and whether `tmp` (file 2) is there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Dir {
    pub target: u8,
    pub tmp: bool,
}

/// File 2's inode block: its size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Size {
    Empty,
    Full,
}

/// The three blocks that change, as the device holds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Disk {
    pub dir: Dir,
    /// None: file 2's inode was never written.
    pub inode: Option<Size>,
    /// File 2's data block holds the new content.
    pub data: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Step {
    Create,
    Write,
    FsyncFile,
    Rename,
    FsyncDir,
    Done,
}

/// The kernel's and the device's side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Kernel {
    /// fsync(2) ends with a device cache flush (UFS SYNCHRONIZE CACHE). Witness `false`: the data sits in the device cache.
    pub flush_on_fsync: bool,
    /// fsync of a new file also writes the folder block naming it (patch 0997, ext2; Linux ext4 does this through the journal).
    pub fsync_writes_new_name: bool,
}

#[derive(Clone, Debug)]
pub struct Replace {
    pub program: Vec<Step>,
    pub kernel: Kernel,
}

impl Replace {
    /// The protocol every LucinaOS program must use to replace a file (spec rule W1).
    pub fn safe() -> Self {
        Replace {
            program: vec![
                Step::Create,
                Step::Write,
                Step::FsyncFile,
                Step::Rename,
                Step::FsyncDir,
                Step::Done,
            ],
            kernel: Kernel {
                flush_on_fsync: true,
                fsync_writes_new_name: true,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct S {
    pub pc: usize,
    pub mem: Disk,
    pub dirty_dir: bool,
    pub dirty_inode: bool,
    pub dirty_data: bool,
    pub durable: Disk,
    pub cached_dir: Option<Dir>,
    pub cached_inode: Option<Size>,
    pub cached_data: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// The program's next step.
    Program(Step),
    /// Writeback writes one dirty block to the device (0 folder, 1 inode, 2 data).
    Writeback(u8),
    /// The device makes one cached block durable on its own (0, 1, 2).
    Drain(u8),
}

/// What the name `target` reads after a cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Content {
    Old,
    New,
    /// Missing, empty or not the data written: what the protocol exists to prevent.
    Broken,
}

impl S {
    fn write(&mut self, block: u8) {
        match block {
            0 => {
                self.cached_dir = Some(self.mem.dir);
                self.dirty_dir = false;
            }
            1 => {
                self.cached_inode = self.mem.inode;
                self.dirty_inode = false;
            }
            _ => {
                self.cached_data = Some(self.mem.data);
                self.dirty_data = false;
            }
        }
    }

    fn drain(&mut self, block: u8) {
        match block {
            0 => {
                if let Some(d) = self.cached_dir.take() {
                    self.durable.dir = d;
                }
            }
            1 => {
                if let Some(i) = self.cached_inode.take() {
                    self.durable.inode = Some(i);
                }
            }
            _ => {
                if let Some(d) = self.cached_data.take() {
                    self.durable.data = d;
                }
            }
        }
    }

    fn flush(&mut self) {
        for b in 0..3 {
            self.drain(b);
        }
    }

    /// Every disk image a power cut can leave now: per block, durable or cached.
    pub fn images(&self) -> Vec<Disk> {
        let dirs: Vec<Dir> = core::iter::once(self.durable.dir)
            .chain(self.cached_dir)
            .collect();
        let inodes: Vec<Option<Size>> = core::iter::once(self.durable.inode)
            .chain(self.cached_inode.map(Some))
            .collect();
        let datas: Vec<bool> = core::iter::once(self.durable.data)
            .chain(self.cached_data)
            .collect();
        let mut out = Vec::new();
        for &dir in &dirs {
            for &inode in &inodes {
                for &data in &datas {
                    out.push(Disk { dir, inode, data });
                }
            }
        }
        out
    }
}

pub fn read_target(d: &Disk) -> Content {
    match d.dir.target {
        1 => Content::Old,
        _ if d.inode == Some(Size::Full) && d.data => Content::New,
        _ => Content::Broken,
    }
}

impl Model for Replace {
    type State = S;
    type Action = Action;

    fn init_states(&self) -> Vec<S> {
        let disk = Disk {
            dir: Dir {
                target: 1,
                tmp: false,
            },
            inode: None,
            data: false,
        };
        vec![S {
            pc: 0,
            mem: disk,
            dirty_dir: false,
            dirty_inode: false,
            dirty_data: false,
            durable: disk,
            cached_dir: None,
            cached_inode: None,
            cached_data: None,
        }]
    }

    fn actions(&self, s: &S, actions: &mut Vec<Action>) {
        if let Some(&step) = self.program.get(s.pc)
            && step != Step::Done
        {
            actions.push(Action::Program(step));
        }
        for (b, dirty) in [s.dirty_dir, s.dirty_inode, s.dirty_data]
            .into_iter()
            .enumerate()
        {
            if dirty {
                actions.push(Action::Writeback(b as u8));
            }
        }
        for (b, cached) in [
            s.cached_dir.is_some(),
            s.cached_inode.is_some(),
            s.cached_data.is_some(),
        ]
        .into_iter()
        .enumerate()
        {
            if cached {
                actions.push(Action::Drain(b as u8));
            }
        }
    }

    fn next_state(&self, last: &S, action: Action) -> Option<S> {
        let mut s = last.clone();
        match action {
            Action::Writeback(b) => s.write(b),
            Action::Drain(b) => s.drain(b),
            Action::Program(step) => {
                match step {
                    Step::Create => {
                        s.mem.dir.tmp = true;
                        s.mem.inode = Some(Size::Empty);
                        s.dirty_dir = true;
                        s.dirty_inode = true;
                    }
                    Step::Write => {
                        s.mem.data = true;
                        s.mem.inode = Some(Size::Full);
                        s.dirty_data = true;
                        s.dirty_inode = true;
                    }
                    Step::FsyncFile => {
                        if s.dirty_data {
                            s.write(2);
                        }
                        if s.dirty_inode {
                            s.write(1);
                        }
                        let name_on_disk = s.durable.dir.tmp || s.durable.dir.target == 2;
                        if self.kernel.fsync_writes_new_name && !name_on_disk && s.dirty_dir {
                            s.write(0);
                        }
                        if self.kernel.flush_on_fsync {
                            s.flush();
                        }
                    }
                    Step::Rename => {
                        s.mem.dir = Dir {
                            target: 2,
                            tmp: false,
                        };
                        s.dirty_dir = true;
                    }
                    Step::FsyncDir => {
                        if s.dirty_dir {
                            s.write(0);
                        }
                        if self.kernel.flush_on_fsync {
                            s.flush();
                        }
                    }
                    Step::Done => {}
                }
                s.pc += 1;
            }
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("a cut never loses the file", |_, s: &S| {
                s.images().iter().all(|d| read_target(d) != Content::Broken)
            }),
            Property::always(
                "after the folder's fsync the new content survives a cut",
                |m: &Replace, s: &S| {
                    m.program.get(s.pc) != Some(&Step::Done)
                        || s.images().iter().all(|d| read_target(d) == Content::New)
                },
            ),
            Property::sometimes("the program finishes", |m: &Replace, s: &S| {
                m.program.get(s.pc) == Some(&Step::Done)
            }),
            Property::sometimes(
                "a cut can leave the old content after the rename",
                |m: &Replace, s: &S| {
                    m.program[..s.pc].contains(&Step::Rename)
                        && s.images().iter().any(|d| read_target(d) == Content::Old)
                },
            ),
        ]
    }
}

/// sync(2) over mounted file systems; mount 0's flush fails (a removed stick, a device error).
#[derive(Clone, Debug)]
pub struct SyncAll {
    pub mounts: usize,
    /// Before 1116: the mount namespace's sync stopped at the first file system whose sync failed.
    pub stop_at_first_error: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SyncState {
    pub dirty: Vec<bool>,
    pub durable: Vec<bool>,
    /// Which mount the sync is at; None before it starts.
    pub at: Option<usize>,
    pub returned: bool,
}

impl Model for SyncAll {
    type State = SyncState;
    type Action = u8;

    fn init_states(&self) -> Vec<SyncState> {
        vec![SyncState {
            dirty: vec![true; self.mounts],
            durable: vec![false; self.mounts],
            at: None,
            returned: false,
        }]
    }

    fn actions(&self, s: &SyncState, actions: &mut Vec<u8>) {
        if !s.returned {
            actions.push(0);
        }
    }

    fn next_state(&self, last: &SyncState, _: u8) -> Option<SyncState> {
        let mut s = last.clone();
        let i = s.at.map_or(0, |i| i + 1);
        if i >= self.mounts {
            s.returned = true;
            return Some(s);
        }
        s.at = Some(i);
        if i == 0 {
            // The failing one.
            if self.stop_at_first_error {
                s.returned = true;
            }
        } else {
            s.dirty[i] = false;
            s.durable[i] = true;
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always(
                "sync(2) leaves every healthy file system on the disk",
                |_, s: &SyncState| !s.returned || (1..s.durable.len()).all(|i| s.durable[i]),
            ),
            Property::sometimes("sync(2) returns", |_, s: &SyncState| s.returned),
        ]
    }
}
