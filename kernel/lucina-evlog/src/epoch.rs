//! Which epoch of the event log a status belongs to (patch 1370). Pure: the caller keeps the state and gives the inputs.
//!
//! The saved log's boot counter starts over when the region is lost. Its epoch id (`/sys/fs/pstore/status`, `epoch <hex>`)
//! changes then, and the event log counts its own epochs, 0, 1, 2 ..., so that two stretches of the counter do not share keys
//! ([`crate::keys`]). An older kernel gives no id; then only a counter that goes backwards says the region started over.

/// How many store ids the state remembers.
pub const MAX_KNOWN: usize = 16;

/// What is kept between ingests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// The event log's current epoch.
    pub epoch: u32,
    /// The last boot counter seen in the current epoch, if any.
    pub last_boot: Option<u32>,
    /// The store ids seen, oldest first, with the epoch each one maps to. At most [`MAX_KNOWN`].
    pub known: Vec<(u64, u32)>,
}

impl State {
    fn with_id(mut self, id: u64, epoch: u32) -> State {
        self.known.retain(|(known, _)| *known != id);
        self.known.push((id, epoch));
        if self.known.len() > MAX_KNOWN {
            let extra = self.known.len() - MAX_KNOWN;
            self.known.drain(..extra);
        }
        self
    }

    fn epoch_of(&self, id: u64) -> Option<u32> {
        self.known
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, epoch)| *epoch)
    }
}

/// The epoch for an ingest of (store id or none, boot counter), and the state after it.
///
/// - A known id maps to its epoch; if that is the current epoch and the counter is lower than the last seen, the counter
///   started over without a new id (a lost state): the next epoch, and the id maps to it from now on.
/// - An unknown id, when no id is known yet, joins the current epoch.
/// - An unknown id when others are known starts the next epoch.
/// - With no id, a counter lower than the last seen starts the next epoch. An equal counter is the same boot, read again.
pub fn next(state: &State, id: Option<u64>, boot: u32) -> (u32, State) {
    let backwards = state.last_boot.is_some_and(|last| boot < last);
    let (epoch, mut after) = match id {
        None if backwards => bump(state, None),
        None => (state.epoch, state.clone()),
        Some(id) => match state.epoch_of(id) {
            Some(e) if e == state.epoch && backwards => bump(state, Some(id)),
            Some(e) => (e, state.clone()),
            None if state.known.is_empty() => (state.epoch, state.clone().with_id(id, state.epoch)),
            None => bump(state, Some(id)),
        },
    };
    // The current epoch never goes down; the last counter is the current epoch's.
    after.epoch = after.epoch.max(epoch);
    if epoch == after.epoch {
        after.last_boot = Some(boot);
    }
    (epoch, after)
}

/// Binds a store id to an epoch, as when a log from before this code has events and the first id seen belongs to the epoch
/// that [`next`] chose without it.
pub fn bind(state: &State, id: u64, epoch: u32) -> State {
    state.clone().with_id(id, epoch)
}

fn bump(state: &State, id: Option<u64>) -> (u32, State) {
    let epoch = state.epoch.saturating_add(1);
    let mut after = State {
        epoch,
        last_boot: None,
        known: state.known.clone(),
    };
    if let Some(id) = id {
        after = after.with_id(id, epoch);
    }
    (epoch, after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys;

    const A: u64 = 0xaaaa;
    const B: u64 = 0xbbbb;

    fn run(steps: &[(Option<u64>, u32)]) -> Vec<u32> {
        let mut state = State::default();
        steps
            .iter()
            .map(|&(id, boot)| {
                let (e, s) = next(&state, id, boot);
                state = s;
                e
            })
            .collect()
    }

    #[test]
    fn the_first_id_joins_the_current_epoch() {
        let (e, s) = next(&State::default(), Some(A), 5);
        assert_eq!(e, 0);
        assert_eq!(s.known, vec![(A, 0)]);
        assert_eq!(s.last_boot, Some(5));
    }

    #[test]
    fn a_known_id_keeps_its_epoch_and_a_new_one_starts_the_next() {
        assert_eq!(
            run(&[
                (Some(A), 1),
                (Some(A), 2),
                (Some(B), 1),
                (Some(B), 2),
                (Some(A), 3)
            ]),
            vec![0, 0, 1, 1, 0],
            "an old store that comes back maps to its own epoch"
        );
    }

    #[test]
    fn a_lower_counter_without_an_id_starts_a_new_epoch() {
        assert_eq!(
            run(&[(None, 5), (None, 6), (None, 2), (None, 3)]),
            vec![0, 0, 1, 1]
        );
    }

    #[test]
    fn a_lower_counter_with_the_same_id_starts_a_new_epoch_too() {
        let mut state = State::default();
        let mut got = Vec::new();
        for boot in [4, 5, 1, 2] {
            let (e, s) = next(&state, Some(A), boot);
            state = s;
            got.push(e);
        }
        assert_eq!(got, vec![0, 0, 1, 1]);
        assert_eq!(state.known, vec![(A, 1)]);
    }

    #[test]
    fn an_equal_counter_is_the_same_boot_read_again() {
        let mut state = State::default();
        for boot in [3, 3, 3] {
            let (e, s) = next(&state, Some(A), boot);
            assert_eq!(e, 0);
            state = s;
        }
        assert_eq!(run(&[(None, 7), (None, 7)]), vec![0, 0]);
    }

    #[test]
    fn the_same_input_twice_gives_the_same_epoch() {
        let mut state = State::default();
        for (id, boot) in [
            (Some(A), 1),
            (Some(B), 1),
            (None, 1),
            (Some(B), 2),
            (None, 1),
        ] {
            let (e1, s1) = next(&state, id, boot);
            let (e2, s2) = next(&s1, id, boot);
            assert_eq!(e1, e2, "{id:?} {boot}");
            assert_eq!(s1, s2);
            state = s1;
        }
    }

    #[test]
    fn the_current_epoch_never_decreases() {
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let mut state = State::default();
        for _ in 0..5000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let id = match x % 5 {
                0 => None,
                n => Some(n),
            };
            let boot = ((x >> 8) % 9) as u32;
            let (e, after) = next(&state, id, boot);
            assert!(after.epoch >= state.epoch);
            assert!(e <= after.epoch);
            state = after;
        }
    }

    #[test]
    fn at_most_16_ids_are_kept_and_the_oldest_goes() {
        let mut state = State::default();
        for id in 1..=20u64 {
            state = next(&state, Some(id), 1).1;
        }
        assert_eq!(state.known.len(), MAX_KNOWN);
        assert_eq!(state.known[0].0, 5);
        assert_eq!(state.known.last().unwrap().0, 20);
        // A forgotten id is unknown: it starts an epoch.
        let (e, _) = next(&state, Some(1), 1);
        assert_eq!(e, state.epoch + 1);
    }

    #[test]
    fn keys_never_collide_across_epochs() {
        let mut seen = std::collections::HashSet::new();
        for epoch in 0..4u32 {
            for boot in 0..6u32 {
                for key in [
                    keys::boot_key(epoch, boot),
                    keys::end_key(epoch, boot),
                    keys::saved_key(epoch, boot, 1),
                    keys::saved_key(epoch, boot, 2),
                ] {
                    assert!(seen.insert(key.clone()), "{key} repeats");
                }
            }
        }
    }

    #[test]
    fn bind_remembers_an_id_for_an_epoch() {
        let s = bind(&State::default(), A, 3);
        assert_eq!(next(&s, Some(A), 1).0, 3);
    }
}
