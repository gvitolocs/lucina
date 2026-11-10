//! What the installer may write where, as plain arithmetic on block numbers. Verus proves this very file, not a copy of it
//! (`verification/juno-disk/rules.rs`; the attributes are only seen by Verus, rustc ignores them). What each function promises is
//! in its `ensures`.
//!
//! Not proved here: reading the fields out of the bytes, the CRC-32 itself, the order of the writes (the tests of `gptw` and the
//! power cuts of `scripts/qemu-juno-disk.sh` check that), or that the device does what it is told.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// The fields of a GPT header that say where things are.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fields {
    pub header_size: u32,
    pub my_lba: u64,
    pub other_lba: u64,
    pub first_usable: u64,
    pub last_usable: u64,
    pub array_lba: u64,
    pub entries: u32,
    pub entry_size: u32,
}

/// Why a header's fields are refused.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bad {
    HeaderSize,
    Address,
    ArraySize,
    ArrayPlace,
    Usable,
}

/// The table as `gptw::plan` adds it. Every number is a block of LUN 0.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub a_first: u64,
    pub a_last: u64,
    pub b_first: u64,
    pub b_last: u64,
    pub data_first: u64,
    pub data_last: u64,
}

/// A root partition.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    A,
    B,
}

#[cfg(verus_keep_ghost)]
verus! {

/// One copy of the table, sound on its own: the CRC's span (`header_size`, which covers every field and lies inside one block),
/// the array's size, both addresses, and an array and a usable area that overlap neither each other nor a header.
pub open spec fn copy_sound(f: Fields, disk_blocks: int, primary: bool, bytes: int, room: int) -> bool {
    let last = disk_blocks - 1;
    let blocks = (bytes + 4095) / 4096;
    &&& 92 <= f.header_size <= 4096
    &&& f.entry_size == 128
    &&& bytes == f.entries * 128
    &&& 0 < bytes <= room
    &&& if primary {
        &&& f.my_lba == 1
        &&& f.other_lba == last
        &&& f.array_lba == 2
    } else {
        &&& f.my_lba == last
        &&& f.other_lba == 1
        &&& f.array_lba + blocks == last
    }
    &&& 2 + blocks <= f.first_usable <= f.last_usable
    &&& f.last_usable + blocks < last
}

/// What `layout` promises: three partitions on 1 MiB boundaries, one after the other, starting right behind the ESP and ending
/// inside the usable area. So none touches the ESP, another partition, or anything of the table.
pub open spec fn layout_sound(l: Layout, esp_last: int, root_blocks: int, first_usable: int, last_usable: int) -> bool {
    &&& l.a_first == esp_last + 1
    &&& l.a_first >= first_usable
    &&& l.a_last == l.a_first + root_blocks - 1
    &&& l.b_first == l.a_last + 1
    &&& l.b_last == l.b_first + root_blocks - 1
    &&& l.data_first == l.b_last + 1
    &&& l.data_last >= l.data_first + 255
    &&& l.data_last <= last_usable
    &&& l.a_first % 256 == 0
    &&& l.b_first % 256 == 0
    &&& l.data_first % 256 == 0
    &&& (l.data_last + 1) % 256 == 0
}

} // verus!

/// The fields of one copy of the table against the disk they were read from. `room` is how many bytes of its array were read.
/// Returns the array's size, which is what its CRC must cover.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r matches Ok(bytes) ==> copy_sound(*f, disk_blocks as int, primary, bytes as int, room as int),
))]
pub fn check_fields(f: &Fields, disk_blocks: u64, primary: bool, room: u64) -> Result<u64, Bad> {
    if f.header_size < 92 || f.header_size > 4096 {
        return Err(Bad::HeaderSize);
    }
    if disk_blocks == 0 {
        return Err(Bad::Address);
    }
    let last = disk_blocks - 1;
    let (my, other) = if primary { (1, last) } else { (last, 1) };
    if f.my_lba != my || f.other_lba != other {
        return Err(Bad::Address);
    }
    if f.entry_size != 128 || f.entries == 0 {
        return Err(Bad::ArraySize);
    }
    let bytes = f.entries as u64 * 128;
    if bytes > room {
        return Err(Bad::ArraySize);
    }
    let blocks = (bytes + 4095) / 4096;
    if blocks >= last {
        return Err(Bad::ArrayPlace);
    }
    let array = if primary { 2 } else { last - blocks };
    if f.array_lba != array {
        return Err(Bad::ArrayPlace);
    }
    if f.first_usable < 2 + blocks || f.first_usable > f.last_usable || f.last_usable >= last - blocks {
        return Err(Bad::Usable);
    }
    Ok(bytes)
}

/// Where the new partitions go. `None`: there is no room, or something would not fall on a 1 MiB boundary.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r matches Some(l) ==> layout_sound(l, esp_last as int, root_blocks as int, first_usable as int, last_usable as int),
))]
pub fn layout(esp_last: u64, root_blocks: u64, first_usable: u64, last_usable: u64) -> Option<Layout> {
    if esp_last >= last_usable || last_usable == u64::MAX || root_blocks == 0 || root_blocks % 256 != 0 {
        return None;
    }
    let start = esp_last + 1;
    if start % 256 != 0 || start < first_usable {
        return None;
    }
    let end = (last_usable + 1) / 256 * 256;
    if end <= start {
        return None;
    }
    let free = end - start;
    if root_blocks > free / 2 {
        return None;
    }
    let roots = 2 * root_blocks;
    if free - roots < 256 {
        return None;
    }
    Some(Layout {
        a_first: start,
        a_last: start + root_blocks - 1,
        b_first: start + root_blocks,
        b_last: start + roots - 1,
        data_first: start + roots,
        data_last: end - 1,
    })
}

/// Whether the table's own writes may touch this block: on the disk, not the protective MBR, and outside the usable area, where
/// the ESP and every partition are.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r <==> (1 <= lba < disk_blocks && (lba < first_usable || lba > last_usable)),
))]
pub fn table_write_allowed(lba: u64, disk_blocks: u64, first_usable: u64, last_usable: u64) -> bool {
    lba >= 1 && lba < disk_blocks && (lba < first_usable || lba > last_usable)
}

/// A partition that may be written whole: behind the ESP and inside the usable area. Returns its size in blocks.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r matches Some(n) ==> esp_last < first <= last <= last_usable && n == last - first + 1,
))]
pub fn writable_partition(first: u64, last: u64, esp_last: u64, last_usable: u64) -> Option<u64> {
    if first <= esp_last || first > last || last > last_usable {
        return None;
    }
    Some(last - first + 1)
}

/// The largest partition this tool writes, in blocks: its byte addresses cannot overflow.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_BLOCKS: u64 = 0x4_0000_0000_0000;

/// One extent of an image against the image and its partition: every byte address it leads to is inside the partition.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r ==> first + count <= image_blocks <= room <= MAX_BLOCKS,
        r ==> (first + count) * 4096 <= u64::MAX,
))]
pub fn extent_fits(first: u64, count: u64, image_blocks: u64, room: u64) -> bool {
    room <= MAX_BLOCKS && image_blocks <= room && first <= image_blocks && count <= image_blocks - first
}

/// The slot a new image goes to: never the one running. When `BOOT.CFG` names neither, the first.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        current matches Some(c) ==> c != r,
))]
pub fn free_slot(current: Option<Slot>) -> Slot {
    match current {
        Some(Slot::A) => Slot::B,
        _ => Slot::A,
    }
}

/// Whether the ESP takes a new kernel and initramfs: what is free now, plus what the run's removal of unnamed generations frees,
/// against both files and the margin. In 128 bits, so no sum can wrap.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r <==> free as int + reclaimable as int >= kernel as int + initrd as int + margin as int,
))]
pub fn esp_has_room(free: u64, reclaimable: u64, kernel: u64, initrd: u64, margin: u64) -> bool {
    free as u128 + reclaimable as u128 >= kernel as u128 + initrd as u128 + margin as u128
}

/// Whether two slots are the same one, by `match`: Verus has no spec for a derived `==`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r <==> a == b,
))]
pub fn same_slot(a: Slot, b: Slot) -> bool {
    match (a, b) {
        (Slot::A, Slot::A) | (Slot::B, Slot::B) => true,
        _ => false,
    }
}

/// Why the root image may not be written.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootRefusal {
    /// The slot is the one `BOOT.CFG` boots.
    BootSlot,
    /// The slot is the one the running system was started from (finding AB-2, docs/spec/juno-ab-boot.md).
    RunningSlot,
    /// The slot is the one `PREV.CFG` boots, and this run does not replace `PREV.CFG`'s set.
    PrevSlot,
    /// The new kernel and initramfs would not fit on the ESP.
    NoRoom,
}

/// The last word before the first write of an install (2026-11-08: the laptop's ESP was found full only after the root image had
/// gone over the slot `PREV.CFG` booted, which left the way back without its root). The caller has run every other check of the
/// run before it. `boot` and `prev` are the slots the two configurations boot, when they name one, and `running` the one the running system
/// came from (the root before a reload into the installer); `replaces_prev` says the run
/// ends by writing `PREV.CFG` anew (an install does: the old `BOOT.CFG` becomes it).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r is Ok ==> (boot matches Some(b) ==> slot != b),
        r is Ok ==> (running matches Some(x) ==> slot != x),
        r is Ok ==> (prev matches Some(p) ==> (p == slot ==> replaces_prev)),
        r is Ok ==> free as int + reclaimable as int >= kernel as int + initrd as int + margin as int,
))]
// Plain `if`s, not let-chains: Verus reads this file as it is.
#[allow(clippy::too_many_arguments, clippy::collapsible_if)]
pub fn may_write_root(
    slot: Slot,
    boot: Option<Slot>,
    running: Option<Slot>,
    prev: Option<Slot>,
    replaces_prev: bool,
    free: u64,
    reclaimable: u64,
    kernel: u64,
    initrd: u64,
    margin: u64,
) -> Result<(), RootRefusal> {
    if let Some(b) = boot {
        if same_slot(b, slot) {
            return Err(RootRefusal::BootSlot);
        }
    }
    if let Some(x) = running {
        if same_slot(x, slot) {
            return Err(RootRefusal::RunningSlot);
        }
    }
    if let Some(p) = prev {
        if same_slot(p, slot) && !replaces_prev {
            return Err(RootRefusal::PrevSlot);
        }
    }
    if !esp_has_room(free, reclaimable, kernel, initrd, margin) {
        return Err(RootRefusal::NoRoom);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISK: u64 = 31191040;
    const LAST: u64 = DISK - 1;

    fn fields(primary: bool) -> Fields {
        Fields {
            header_size: 92,
            my_lba: if primary { 1 } else { LAST },
            other_lba: if primary { LAST } else { 1 },
            first_usable: 6,
            last_usable: LAST - 5,
            array_lba: if primary { 2 } else { LAST - 4 },
            entries: 128,
            entry_size: 128,
        }
    }

    #[test]
    fn the_laptops_table_passes_and_each_bad_field_is_refused() {
        assert_eq!(check_fields(&fields(true), DISK, true, 16384), Ok(16384));
        assert_eq!(check_fields(&fields(false), DISK, false, 16384), Ok(16384));
        let bad = |change: &dyn Fn(&mut Fields), why| {
            let mut f = fields(true);
            change(&mut f);
            assert_eq!(check_fields(&f, DISK, true, 16384), Err(why));
        };
        bad(&|f| f.header_size = 91, Bad::HeaderSize);
        bad(&|f| f.header_size = 4097, Bad::HeaderSize);
        bad(&|f| f.my_lba = 2, Bad::Address);
        bad(&|f| f.other_lba = 2, Bad::Address);
        bad(&|f| f.entry_size = 256, Bad::ArraySize);
        bad(&|f| f.entries = 129, Bad::ArraySize);
        bad(&|f| f.entries = 0, Bad::ArraySize);
        bad(&|f| f.array_lba = 3, Bad::ArrayPlace);
        bad(&|f| f.first_usable = 5, Bad::Usable);
        bad(&|f| f.last_usable = LAST - 4, Bad::Usable);
        assert_eq!(check_fields(&fields(true), 0, true, 16384), Err(Bad::Address));
    }

    #[test]
    fn the_laptops_layout() {
        assert_eq!(
            layout(66815, 8 << 18, 6, LAST - 5),
            Some(Layout {
                a_first: 66816,
                a_last: 2163967,
                b_first: 2163968,
                b_last: 4261119,
                data_first: 4261120,
                data_last: 31190783,
            })
        );
        assert_eq!(layout(66815, 64 << 18, 6, LAST - 5), None);
        assert_eq!(layout(66816, 8 << 18, 6, LAST - 5), None);
        assert_eq!(layout(66815, 100, 6, LAST - 5), None);
        assert_eq!(layout(66815, u64::MAX / 256 * 256, 6, LAST - 5), None);
        assert_eq!(layout(66815, 8 << 18, 70000, LAST - 5), None);
    }

    #[test]
    fn writes_stay_out_of_the_usable_area_and_on_the_disk() {
        let allowed = |lba| table_write_allowed(lba, DISK, 6, LAST - 5);
        for lba in [1, 2, 5, LAST - 4, LAST] {
            assert!(allowed(lba), "{lba}");
        }
        for lba in [0, 6, 256, 66815, 66816, LAST - 5, DISK, u64::MAX] {
            assert!(!allowed(lba), "{lba}");
        }
    }

    #[test]
    fn partitions_extents_and_slots() {
        assert_eq!(writable_partition(66816, 2163967, 66815, LAST - 5), Some(2097152));
        assert_eq!(writable_partition(256, 66815, 66815, LAST - 5), None);
        assert_eq!(writable_partition(66816, 66815, 66815, LAST - 5), None);
        assert_eq!(writable_partition(66816, LAST - 4, 66815, LAST - 5), None);
        assert!(extent_fits(0, 10, 10, 10));
        assert!(!extent_fits(1, 10, 10, 10));
        assert!(!extent_fits(u64::MAX, 2, 10, 10));
        assert!(!extent_fits(0, 11, 11, 10));
        assert_eq!(free_slot(None), Slot::A);
        assert_eq!(free_slot(Some(Slot::A)), Slot::B);
        assert_eq!(free_slot(Some(Slot::B)), Slot::A);
    }

    #[test]
    fn the_laptops_full_esp_is_refused_before_the_root_is_written() {
        // 2026-11-08: 47034368 bytes free, nothing left to remove, a 47873505-byte need (kernel + initramfs + the 1 MiB margin).
        let (kernel, initrd, margin) = (10_250_881, 36_574_048, 1 << 20);
        assert_eq!(kernel + initrd + margin, 47_873_505);
        let laptop = may_write_root(Slot::A, Some(Slot::B), Some(Slot::B), Some(Slot::A), true, 47_034_368, 0, kernel, initrd, margin);
        assert_eq!(laptop, Err(RootRefusal::NoRoom));
        // The same run once the dead generation's 43 MB are counted as freed by the run's own removal.
        assert_eq!(may_write_root(Slot::A, Some(Slot::B), Some(Slot::B), Some(Slot::A), true, 47_034_368, 43_315_560, kernel, initrd, margin), Ok(()));
    }

    #[test]
    fn slots_and_room() {
        let ok = |slot, boot, prev, replaces| may_write_root(slot, boot, boot, prev, replaces, 100, 0, 10, 10, 10);
        assert_eq!(ok(Slot::A, Some(Slot::A), None, true), Err(RootRefusal::BootSlot));
        assert_eq!(ok(Slot::B, Some(Slot::B), Some(Slot::A), true), Err(RootRefusal::BootSlot));
        assert_eq!(ok(Slot::A, Some(Slot::B), Some(Slot::A), false), Err(RootRefusal::PrevSlot));
        assert_eq!(ok(Slot::A, Some(Slot::B), Some(Slot::A), true), Ok(()));
        assert_eq!(ok(Slot::A, Some(Slot::B), Some(Slot::B), false), Ok(()));
        assert_eq!(ok(Slot::A, Some(Slot::B), None, false), Ok(()));
        assert_eq!(ok(Slot::A, None, None, false), Ok(()));
        // AB-2: after a settle rollback BOOT.CFG names A while B still runs; the free slot is then the running one.
        assert_eq!(may_write_root(Slot::B, Some(Slot::A), Some(Slot::B), None, true, 100, 0, 10, 10, 10), Err(RootRefusal::RunningSlot));
        assert!(esp_has_room(30, 0, 10, 10, 10));
        assert!(!esp_has_room(29, 0, 10, 10, 10));
        assert!(esp_has_room(10, 20, 10, 10, 10));
        assert!(esp_has_room(u64::MAX, u64::MAX, u64::MAX, u64::MAX, 0));
        assert!(!esp_has_room(u64::MAX, u64::MAX, u64::MAX, u64::MAX, 1));
        assert!(!esp_has_room(u64::MAX, 0, u64::MAX, 1, 0));
    }
}
