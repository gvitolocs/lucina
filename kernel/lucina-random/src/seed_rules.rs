//! The order of the steps with the seed file on `/data`: **a seed is mixed in only after it is gone from the disk**, so a
//! power cut at any moment cannot make the next boot use it again.
//!
//! This is systemd-boot's rule (it writes the new seed before it hands one to the kernel), for a file that the kernel never
//! reads. The state moves through [`next_step`]; `lucina-data` follows it, and the bytes are in `seed_file`.
//!
//! Verus proves this file as shipped (`verification/juno-random/rules.rs`). Not proved: what the file system does with an
//! fsync, and that the disk tells the truth.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// What the seed file held at boot.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found {
    Missing,
    /// The wrong length: nothing to use.
    Damaged,
    Whole,
}

/// How far a boot has come.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Load {
    pub found: Found,
    /// The file on the disk is not the seed that was found, and that has reached the disk.
    pub replaced: bool,
    /// Replacing it failed: the old seed may still be read at the next boot.
    pub replace_failed: bool,
    pub mixed: bool,
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Write the next seed to a new file, fsync, rename over the old one, fsync the folder.
    Replace,
    /// Give the kernel what the old seed yields.
    Mix,
    /// Nothing more; the old seed was not used.
    Skip,
    Done,
}

#[cfg(verus_keep_ghost)]
verus! {

/// What a boot's state may be: mixed only if found whole and replaced first.
pub open spec fn safe(l: Load) -> bool {
    l.mixed ==> (l.found == Found::Whole && l.replaced)
}

} // verus!

/// The state of a boot before any step.
#[cfg_attr(verus_keep_ghost, verus_spec(l =>
    ensures
        safe(l),
        !l.replaced && !l.replace_failed && !l.mixed,
        l.found == Found::Whole <==> file_len == Some(want),
))]
pub fn start(file_len: Option<usize>, want: usize) -> Load {
    let found = match file_len {
        None => Found::Missing,
        Some(len) => {
            if len == want {
                Found::Whole
            } else {
                Found::Damaged
            }
        }
    };
    Load { found, replaced: false, replace_failed: false, mixed: false }
}

/// What comes next. **`Step::Mix` only on a whole seed that was replaced, and only once**; a failed replacement never mixes.
#[cfg_attr(verus_keep_ghost, verus_spec(s =>
    requires
        safe(l),
    ensures
        s == Step::Mix ==> l.found == Found::Whole && l.replaced && !l.mixed,
        s == Step::Replace ==> l.found == Found::Whole && !l.replaced && !l.replace_failed,
        l.replace_failed && !l.replaced ==> s != Step::Mix,
        l.found != Found::Whole ==> s != Step::Mix && s != Step::Replace,
        // Always somewhere to go: a whole seed that was neither replaced nor refused has its replacement next.
        l.found == Found::Whole && !l.replaced && !l.replace_failed ==> s == Step::Replace,
        l.found == Found::Whole && l.replaced && !l.mixed ==> s == Step::Mix,
))]
pub fn next_step(l: Load) -> Step {
    match l.found {
        Found::Missing | Found::Damaged => Step::Skip,
        Found::Whole => {
            if l.mixed {
                Step::Done
            } else if l.replaced {
                Step::Mix
            } else if l.replace_failed {
                Step::Skip
            } else {
                Step::Replace
            }
        }
    }
}

/// After [`Step::Replace`] reached the disk. The old seed has not been used yet.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        safe(l),
        l.found == Found::Whole,
        !l.mixed,
    ensures
        safe(r),
        r.replaced,
        !r.mixed,
        r.found == l.found,
))]
pub fn replaced(l: Load) -> Load {
    Load { replaced: true, ..l }
}

/// After [`Step::Replace`] failed.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        safe(l),
    ensures
        safe(r),
        r.replace_failed,
        r.mixed == l.mixed,
        r.replaced == l.replaced,
        r.found == l.found,
))]
pub fn replace_failed(l: Load) -> Load {
    Load { replace_failed: true, ..l }
}

/// After [`Step::Mix`]. Requires what [`next_step`] promises, so it keeps [`safe`].
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        safe(l),
        l.found == Found::Whole,
        l.replaced,
    ensures
        safe(r),
        r.mixed,
))]
pub fn mixed(l: Load) -> Load {
    Load { mixed: true, ..l }
}

/// Whether the seed file may be written with new output of the kernel, at any time after the boot's steps: only from a seeded
/// generator, or the file would be worth less than the one it replaces.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r <==> kernel_seeded,
))]
pub fn may_save(kernel_seeded: bool) -> bool {
    kernel_seeded
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every order the driver can see: the seed is mixed once at most, and never before it is replaced.
    #[test]
    fn a_whole_seed_goes_replace_then_mix_then_done() {
        let mut l = start(Some(32), 32);
        assert_eq!(next_step(l), Step::Replace);
        l = replaced(l);
        assert_eq!(next_step(l), Step::Mix);
        l = mixed(l);
        assert_eq!(next_step(l), Step::Done);
    }

    #[test]
    fn a_failed_replacement_never_mixes() {
        let l = replace_failed(start(Some(32), 32));
        assert_eq!(next_step(l), Step::Skip);
    }

    #[test]
    fn no_file_or_a_damaged_one_has_nothing_to_mix() {
        assert_eq!(next_step(start(None, 32)), Step::Skip);
        assert_eq!(next_step(start(Some(31), 32)), Step::Skip);
        assert_eq!(next_step(start(Some(0), 32)), Step::Skip);
    }
}
