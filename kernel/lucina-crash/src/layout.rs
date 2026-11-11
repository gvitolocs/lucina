// Where the saved log's zones are in a region of a given size, for each format (patch 1213). Verus checks this very file:
// `//@` lines hold the proof, see scripts/verify-shipped.sh (D-011); verification/juno-crash-layout/mutants.sh plants bugs
// that Verus must refuse. store.rs calls these functions and holds no copy of the arithmetic.
//
// Format 3 split what the records leave between two console zones; the 4 KiB after them held the register trace.
// Format 4 keeps every record where it was and the first console zone's start, shrinks both console zones, and leaves
// TAIL_RESERVE more at the end for a larger trace, idle stamps and spin-lock waits. So a format-3 region's records read the
// same under format 4, and only the consoles move.

/// Bytes before the first record: the header, the state, and room.
pub const FIRST_ZONE: usize = 4096;
/// Record slots.
pub const RECORDS: usize = 8;
/// The smallest region the store takes.
pub const MIN_SIZE: usize = 64 * 1024;
/// What format 4 adds to the tail, in a region of at least [`RESERVE_FROM`].
pub const TAIL_RESERVE: usize = 64 * 1024;
/// Below this, format 4 lays a region out as format 3 did.
pub const RESERVE_FROM: usize = 256 * 1024;
/// Largest region handled: 1 GiB (the laptop's is 2 MiB).
pub const MAX_SIZE: usize = 1 << 30;

//@ pub open spec fn record_spec(size: int) -> int { (size / 16) / 4096 * 4096 }
//@ pub open spec fn reserve_spec(size: int, version: int) -> int {
//@     if version >= 4 && size >= 262144 { 65536 } else { 0 }
//@ }
//@ pub open spec fn console_spec(size: int, version: int) -> int {
//@     ((size - 4096 - 8 * record_spec(size) - reserve_spec(size, version)) / 2) / 4096 * 4096
//@ }
//@ pub open spec fn tail_spec(size: int, version: int) -> int {
//@     4096 + 8 * record_spec(size) + 2 * console_spec(size, version)
//@ }

/// One record slot's bytes: a sixteenth of the region, in whole pages.
pub fn record_size(size: usize) -> usize //@ret
//@ requires 65536 <= size <= 1073741824,
//@ ensures ret == record_spec(size as int), ret % 4096 == 0, ret * 16 <= size, ret >= 4096,
{
    //@ assert(size / 16 >= 4096) by(nonlinear_arith) requires size >= 65536;
    //@ assert((size / 16) / 4096 * 4096 <= size / 16) by(nonlinear_arith);
    //@ assert((size / 16) / 4096 * 4096 >= 4096) by(nonlinear_arith) requires size / 16 >= 4096;
    //@ assert(((size / 16) / 4096 * 4096) % 4096 == 0) by(nonlinear_arith);
    //@ assert((size / 16) * 16 <= size) by(nonlinear_arith);
    //@ assert((size / 16) / 4096 * 4096 * 16 <= size) by(nonlinear_arith)
    //@     requires (size / 16) / 4096 * 4096 <= size / 16, (size / 16) * 16 <= size;
    (size / 16) / 4096 * 4096
}

/// What format `version` (3 or 4) keeps after the consoles, beyond page rounding.
pub fn reserve(size: usize, version: u32) -> usize //@ret
//@ ensures ret == reserve_spec(size as int, version as int),
{
    if version >= 4 && size >= RESERVE_FROM {
        TAIL_RESERVE
    } else {
        0
    }
}

/// One console zone's bytes, in whole pages.
pub fn console_size(size: usize, version: u32) -> usize //@ret
//@ requires 65536 <= size <= 1073741824,
//@ ensures ret == console_spec(size as int, version as int), ret % 4096 == 0,
//@     4096 + 8 * record_spec(size as int) + reserve_spec(size as int, version as int) + 2 * ret <= size,
{
    let records = record_size(size);
    let reserved = reserve(size, version);
    //@ assert(8 * records <= size / 2) by(nonlinear_arith) requires records * 16 <= size;
    //@ assert(4096 + 8 * records + reserved <= size) by(nonlinear_arith)
    //@     requires 8 * records <= size / 2, size >= 65536, reserved == 0 || (reserved == 65536 && size >= 262144);
    let left = size - FIRST_ZONE - RECORDS * records - reserved;
    //@ assert((left / 2) / 4096 * 4096 * 2 <= left) by(nonlinear_arith);
    //@ assert(((left / 2) / 4096 * 4096) % 4096 == 0) by(nonlinear_arith);
    (left / 2) / 4096 * 4096
}

/// Where the tail starts: after the records and both consoles.
pub fn tail_start(size: usize, version: u32) -> usize //@ret
//@ requires 65536 <= size <= 1073741824,
//@ ensures ret == tail_spec(size as int, version as int), ret <= size,
//@     size - ret >= reserve_spec(size as int, version as int),
{
    let records = record_size(size);
    let consoles = console_size(size, version);
    FIRST_ZONE + RECORDS * records + 2 * consoles
}

/// Format 4 leaves at least as much tail as format 3: its consoles are no larger.
pub fn tail_grows(size: usize) -> bool //@ret
//@ requires 65536 <= size <= 1073741824,
//@ ensures ret,
{
    let v3 = tail_start(size, 3);
    let v4 = tail_start(size, 4);
    //@ let ghost l3: int = (size as int) - 4096 - 8 * record_spec(size as int);
    //@ assert(console_spec(size as int, 4) <= console_spec(size as int, 3)) by(nonlinear_arith)
    //@     requires l3 >= reserve_spec(size as int, 4), reserve_spec(size as int, 4) >= 0,
    //@         console_spec(size as int, 4) == ((l3 - reserve_spec(size as int, 4)) / 2) / 4096 * 4096,
    //@         console_spec(size as int, 3) == (l3 / 2) / 4096 * 4096;
    v4 <= v3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_laptops_region() {
        let size = 2 * 1024 * 1024;
        // Format 3 as it was: 128 KiB records, 508 KiB consoles, 4 KiB tail.
        assert_eq!(
            (record_size(size), console_size(size, 3)),
            (128 * 1024, 508 * 1024)
        );
        assert_eq!(size - tail_start(size, 3), 4096);
        // Format 4: same records, 476 KiB consoles, a 68 KiB tail.
        assert_eq!(console_size(size, 4), 476 * 1024);
        assert_eq!(size - tail_start(size, 4), 68 * 1024);
        assert!(tail_grows(size));
    }

    #[test]
    fn small_regions_keep_format_3s_layout() {
        for size in [MIN_SIZE, 128 * 1024, RESERVE_FROM - 4096] {
            assert_eq!(tail_start(size, 4), tail_start(size, 3), "{size}");
            assert!(tail_grows(size));
        }
    }

    #[test]
    fn the_old_arithmetic_is_the_same() {
        for size in (MIN_SIZE..=4 * 1024 * 1024).step_by(4096 * 7 + 512) {
            assert_eq!(record_size(size), (size / 16) & !4095);
            let left = size - FIRST_ZONE - RECORDS * record_size(size);
            assert_eq!(console_size(size, 3), (left / 2) & !4095);
        }
    }
}
