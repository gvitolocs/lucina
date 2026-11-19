//! The keys that make an event unique among its source's, and the activity an incident's events share, in one place.
//!
//! The saved log's boot counter starts over when its region is lost (a power-off, a rollback). Patch 1370 gives the region an
//! epoch id and [`crate::epoch`] numbers the event log's own epochs from it. Epoch 0 keeps the keys from before, so that
//! the log's existing entries stay valid and read as epoch 0; epoch E > 0 puts `e{E}` in them, so that boot 3 of epoch 1
//! is not boot 3 of epoch 0.

/// The boot-started event of a boot, and the activity its events share: `boot:17`, `boot:e2:17`.
pub fn boot_key(epoch: u32, boot: u32) -> String {
    if epoch == 0 {
        format!("boot:{boot}")
    } else {
        format!("boot:e{epoch}:{boot}")
    }
}

/// The incident that a boot's events belong to.
pub fn activity(epoch: u32, boot: u32) -> String {
    boot_key(epoch, boot)
}

/// A saved record: `pstore:boot17:seq42`, `pstore:e2:boot17:seq42`. The sequence number and the boot never repeat together
/// within an epoch.
pub fn saved_key(epoch: u32, boot: u32, seq: u32) -> String {
    if epoch == 0 {
        format!("pstore:boot{boot}:seq{seq}")
    } else {
        format!("pstore:e{epoch}:boot{boot}:seq{seq}")
    }
}

/// The clean end of a boot: `end:boot17`, `end:e2:boot17`.
pub fn end_key(epoch: u32, boot: u32) -> String {
    if epoch == 0 {
        format!("end:boot{boot}")
    } else {
        format!("end:e{epoch}:boot{boot}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_0_keeps_the_keys_from_before() {
        assert_eq!(boot_key(0, 17), "boot:17");
        assert_eq!(activity(0, 17), "boot:17");
        assert_eq!(saved_key(0, 17, 42), "pstore:boot17:seq42");
        assert_eq!(end_key(0, 17), "end:boot17");
    }

    #[test]
    fn later_epochs_say_so() {
        assert_eq!(boot_key(2, 17), "boot:e2:17");
        assert_eq!(saved_key(2, 17, 42), "pstore:e2:boot17:seq42");
        assert_eq!(end_key(2, 17), "end:e2:boot17");
    }
}
