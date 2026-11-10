use alloc::vec::Vec;

use dma_api::{CoherentArray, DmaDirection};
use mbarrier::rmb;
use xhci::ring::trb::event::Allowed;

use super::ring::{Ring, TRBS_PER_SEGMENT};
use crate::{err::*, osal::Kernel};

#[repr(C)]
#[derive(Clone, Copy)]
struct EventRingSte {
    addr: u64,
    size: u16,
    _reserved: [u8; 6],
}

pub(crate) struct EventRing {
    segments: Vec<Ring>,
    segment_index: usize,
    trb_index: usize,
    cycle: bool,
    ste: CoherentArray<EventRingSte>,
}

/// Ours, from crab-usb: two segments of `TRBS_PER_SEGMENT` (256) TRBs; Linux uses one segment.
const EVENT_RING_SEGMENTS: usize = 2;
/// The cycle bit, in the last word of a TRB.
const TRB_CYCLE: u32 = 1;

/// Reads an event TRB if it is the controller's new one, as Linux's `xhci_handle_event`: the word with the cycle bit,
/// a read barrier, and only then the rest. The controller writes that word last, so what follows is complete. Read
/// the other way round, on memory the CPU does not snoop for the device, the pointer can be the previous lap's while
/// the cycle bit is already the new one: an event that names one endpoint and a TRB of another.
fn read_event_words(
    mut read_word: impl FnMut(usize) -> u32,
    mut read_barrier: impl FnMut(),
    cycle: bool,
) -> Option<[u32; 4]> {
    let control = read_word(3);
    if (control & TRB_CYCLE != 0) != cycle {
        return None;
    }
    read_barrier();
    Some([read_word(0), read_word(1), read_word(2), control])
}

impl EventRing {
    pub fn new(max_segments: usize, dma: &Kernel) -> Result<Self> {
        let segment_count = EVENT_RING_SEGMENTS.min(max_segments.max(1));
        let mut segments = Vec::with_capacity(segment_count);
        for _ in 0..segment_count {
            segments.push(Ring::new_segment(false, DmaDirection::Bidirectional, dma)?);
        }

        let mut ste = dma
            .coherent_array_zero_with_align(segment_count, 64)
            .map_err(|_| USBError::NoMemory)?;

        for (index, segment) in segments.iter().enumerate() {
            ste.set_cpu(
                index,
                EventRingSte {
                    addr: segment.bus_addr().raw(),
                    size: TRBS_PER_SEGMENT as _,
                    _reserved: [0; 6],
                },
            );
        }

        Ok(Self {
            segments,
            segment_index: 0,
            trb_index: 0,
            cycle: true,
            ste,
        })
    }

    pub fn next(&mut self) -> Option<Allowed> {
        loop {
            let raw = self.read_current()?;
            self.inc_deque();
            match Allowed::try_from(raw) {
                Ok(allowed) => return Some(allowed),
                // It is the controller's and it is consumed: stopping here would stop the ring for good.
                Err(_) => warn!("xhci: unknown event TRB {raw:08x?} skipped"),
            }
        }
    }

    pub fn has_pending_event(&mut self) -> bool {
        (self.current_word(3) & TRB_CYCLE != 0) == self.cycle
    }

    pub fn erdp(&self) -> u64 {
        self.current_segment().trb_bus_addr(self.trb_index).raw() & 0xFFFF_FFFF_FFFF_FFF0
    }

    pub fn erst_dequeue_pointer(&self) -> u64 {
        self.erdp() | (self.segment_index as u64 & 0x7)
    }

    pub fn segment_index(&self) -> u8 {
        (self.segment_index & 0x7) as u8
    }

    pub fn erstba(&self) -> u64 {
        self.ste.dma_addr().as_u64()
    }

    pub fn len(&self) -> usize {
        self.ste.len()
    }

    pub fn info(&self) -> EventRingInfo {
        EventRingInfo {
            erstz: self.len() as _,
            erdp: self.erst_dequeue_pointer(),
            erstba: self.erstba(),
        }
    }

    fn current_segment(&self) -> &Ring {
        &self.segments[self.segment_index]
    }

    fn current_word(&self, word: usize) -> u32 {
        self.current_segment().read_word(self.trb_index, word)
    }

    fn read_current(&self) -> Option<[u32; 4]> {
        read_event_words(|word| self.current_word(word), rmb, self.cycle)
    }

    fn inc_deque(&mut self) {
        self.trb_index += 1;
        if self.trb_index >= TRBS_PER_SEGMENT {
            self.trb_index = 0;
            self.segment_index += 1;
            if self.segment_index >= self.segments.len() {
                self.segment_index = 0;
                self.cycle = !self.cycle;
            }
        }
    }
}

pub(crate) struct EventRingInfo {
    pub(crate) erstz: u16,
    pub(crate) erdp: u64,
    pub(crate) erstba: u64,
}

#[cfg(test)]
mod lucina_tests {
    use core::cell::{Cell, RefCell};

    use super::read_event_words;
    use crate::backend::kmod::xhci::ring::write_trb_words;

    const OLD: [u32; 4] = [0xaaaa_0000, 0x1, 0, (32 << 10) | 0]; // the previous lap: another endpoint's TRB, cycle 0
    const NEW: [u32; 4] = [0xbbbb_0000, 0x1, 0, (32 << 10) | 1]; // the event being written, cycle 1

    /// A slot of the event ring that the controller overwrites, words in order with the cycle word last, just before
    /// the CPU's read number `at`.
    fn slot(at: usize) -> (RefCell<[u32; 4]>, Cell<usize>, usize) {
        (RefCell::new(OLD), Cell::new(0), at)
    }

    fn read((memory, reads, at): &(RefCell<[u32; 4]>, Cell<usize>, usize), word: usize) -> u32 {
        if reads.get() == *at {
            *memory.borrow_mut() = NEW;
        }
        reads.set(reads.get() + 1);
        memory.borrow()[word]
    }

    /// crab-usb 0.12.1's order, kept to show the failure: read the TRB in one go, then look at its cycle bit.
    fn published_order(slot: &(RefCell<[u32; 4]>, Cell<usize>, usize)) -> Option<[u32; 4]> {
        let raw = [read(slot, 0), read(slot, 1), read(slot, 2), read(slot, 3)];
        (raw[3] & 1 == 1).then_some(raw)
    }

    /// The laptop's fault: the new cycle bit with the previous lap's pointer.
    #[test]
    fn published_order_can_pair_a_new_cycle_bit_with_an_old_pointer() {
        let torn = (0..=4)
            .filter_map(|at| published_order(&slot(at)))
            .find(|raw| *raw != NEW);
        assert_eq!(torn, Some([OLD[0], OLD[1], OLD[2], NEW[3]]));
    }

    #[test]
    fn the_cycle_word_first_never_pairs_them() {
        for at in 0..=4 {
            let slot = slot(at);
            let got = read_event_words(|word| read(&slot, word), || {}, true);
            assert!(
                got.is_none() || got == Some(NEW),
                "write before read {at}: {got:x?}"
            );
        }
    }

    #[test]
    fn the_barrier_is_between_the_cycle_word_and_the_rest() {
        let order = RefCell::new(alloc::vec::Vec::new());
        read_event_words(
            |word| {
                order.borrow_mut().push(word as i32);
                NEW[word]
            },
            || order.borrow_mut().push(-1),
            true,
        );
        assert_eq!(*order.borrow(), [3, -1, 0, 1, 2]);
    }

    #[test]
    fn an_old_trb_stops_the_read_before_the_barrier() {
        let barriers = Cell::new(0);
        let got = read_event_words(|word| OLD[word], || barriers.set(barriers.get() + 1), true);
        assert_eq!((got, barriers.get()), (None, 0));
    }

    #[test]
    fn a_trb_is_written_with_its_cycle_word_last() {
        let order = RefCell::new(alloc::vec::Vec::new());
        write_trb_words(
            NEW,
            |word, _| order.borrow_mut().push(word as i32),
            || order.borrow_mut().push(-1),
        );
        assert_eq!(*order.borrow(), [0, 1, 2, -1, 3]);
    }
}
