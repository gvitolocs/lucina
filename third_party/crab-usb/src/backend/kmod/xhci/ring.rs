use alloc::vec::Vec;

use dma_api::{CoherentArray, DmaDirection};
use mbarrier::{mb, wmb};
use xhci::ring::trb::{Link, command, transfer};

use crate::{
    BusAddr,
    err::*,
    osal::Kernel,
    queue::{Finished, TWaiter},
};

const TRB_LEN: usize = 4;
pub(crate) const TRB_SIZE: usize = size_of::<TrbData>();
pub(crate) const TRBS_PER_SEGMENT: usize = 256;
/// Ours, from crab-usb: two 4 KiB pages, 512 TRBs with the Link TRB; Linux's rings start with one segment of
/// `TRBS_PER_SEGMENT` (256) TRBs.
const DEFAULT_RING_PAGES: usize = 2;

#[derive(Clone, Copy)]
#[repr(transparent)]
pub(crate) struct TrbData([u32; TRB_LEN]);

impl TrbData {
    pub fn to_raw(self) -> [u32; TRB_LEN] {
        self.0
    }
}

impl From<command::Allowed> for TrbData {
    fn from(value: command::Allowed) -> Self {
        let raw = value.into_raw();
        Self(raw)
    }
}

impl From<transfer::Allowed> for TrbData {
    fn from(value: transfer::Allowed) -> Self {
        let raw = value.into_raw();
        Self(raw)
    }
}

/// Writes a TRB so that the controller cannot see its cycle bit before the rest, as Linux's `queue_trb`: the first
/// three words, a write barrier, then the word with the cycle bit. The controller may be reading ahead on this ring,
/// and a TRB that looks valid with the buffer pointer of its previous lap would send it to freed memory.
pub(crate) fn write_trb_words(
    trb: [u32; TRB_LEN],
    mut write_word: impl FnMut(usize, u32),
    mut write_barrier: impl FnMut(),
) {
    write_word(0, trb[0]);
    write_word(1, trb[1]);
    write_word(2, trb[2]);
    write_barrier();
    write_word(3, trb[3]);
}

pub(crate) struct Ring {
    link: bool,
    trbs: CoherentArray<TrbData>,
    i: usize,
    cycle: bool,
}

impl Ring {
    pub fn new_with_len(
        len: usize,
        link: bool,
        direction: DmaDirection,
        dma: &Kernel,
    ) -> core::result::Result<Self, HostError> {
        let _ = direction;
        let trbs = dma.coherent_array_zero_with_align(len, dma.page_size())?;

        Ok(Self {
            link,
            trbs,
            i: 0,
            cycle: link,
        })
    }

    pub fn new(link: bool, direction: DmaDirection, dma: &Kernel) -> Result<Self> {
        Self::new_with_pages(DEFAULT_RING_PAGES, link, direction, dma)
    }

    pub fn new_with_pages(
        pages: usize,
        link: bool,
        direction: DmaDirection,
        dma: &Kernel,
    ) -> Result<Self> {
        let len = (dma.page_size() * pages.max(1)) / TRB_SIZE;
        Ok(Self::new_with_len(len, link, direction, dma)?)
    }

    pub fn new_segment(link: bool, direction: DmaDirection, dma: &Kernel) -> Result<Self> {
        Ok(Self::new_with_len(TRBS_PER_SEGMENT, link, direction, dma)?)
    }

    pub fn len(&self) -> usize {
        self.trbs.len()
    }

    pub fn bus_addr(&self) -> BusAddr {
        self.trbs.dma_addr().as_u64().into()
    }

    /// Reads one word of a TRB. Volatile: the controller writes this memory.
    pub(crate) fn read_word(&self, index: usize, word: usize) -> u32 {
        assert!(index < self.len() && word < TRB_LEN);
        // SAFETY: the indices are in bounds of the allocation, which lives as long as `self`, and `TrbData` is
        // four `u32`s.
        unsafe {
            self.trbs
                .as_ptr()
                .as_ptr()
                .add(index)
                .cast::<u32>()
                .add(word)
                .read_volatile()
        }
    }

    fn write_trb(&mut self, index: usize, trb: TrbData) {
        assert!(index < self.len());
        let base = self.trbs.as_ptr().as_ptr();
        write_trb_words(
            trb.to_raw(),
            |word, value| {
                // SAFETY: as in `read_word`.
                unsafe {
                    base.add(index)
                        .cast::<u32>()
                        .add(word)
                        .write_volatile(value)
                }
            },
            wmb,
        );
    }

    pub fn enque_command(&mut self, mut trb: command::Allowed) -> BusAddr {
        if self.cycle {
            trb.set_cycle_bit();
        } else {
            trb.clear_cycle_bit();
        }
        let addr = self.enque_trb(trb.into());
        trace!("[CMD] >> {trb:X?} @{addr:X?}");
        addr
    }

    pub fn enque_transfer(&mut self, mut trb: transfer::Allowed) -> BusAddr {
        if self.cycle {
            trb.set_cycle_bit();
        } else {
            trb.clear_cycle_bit();
        }
        let addr = self.enque_trb(trb.into());
        trace!("[Transfer] >> {trb:X?} @{addr:X?}");
        addr
    }

    fn enque_transfer_deferred_cycle(
        &mut self,
        mut trb: transfer::Allowed,
    ) -> (BusAddr, usize, transfer::Allowed) {
        let mut visible_trb = trb;
        if self.cycle {
            visible_trb.set_cycle_bit();
            trb.clear_cycle_bit();
        } else {
            visible_trb.clear_cycle_bit();
            trb.set_cycle_bit();
        }
        let index = self.i;
        let addr = self.enque_trb(trb.into());
        trace!("[Transfer] >> deferred first {visible_trb:X?} @{addr:X?}");
        (addr, index, visible_trb)
    }

    fn set_transfer_trb(&mut self, index: usize, trb: transfer::Allowed) {
        self.write_trb(index, trb.into());
    }

    pub fn enque_trb(&mut self, trb: TrbData) -> BusAddr {
        self.write_trb(self.i, trb);
        let addr = self.trb_bus_addr(self.i);
        self.next_index();
        addr
    }

    fn next_index(&mut self) -> usize {
        self.i += 1;
        let len = self.len();

        // link模式下，最后一个是Link
        if self.link && self.i >= len - 1 {
            self.i = 0;
            trace!("link!");
            let address = self.trb_bus_addr(0);
            let mut link = Link::new();
            link.set_ring_segment_pointer(address.into())
                .set_toggle_cycle();

            if self.cycle {
                link.set_cycle_bit();
            } else {
                link.clear_cycle_bit();
            }
            let trb = command::Allowed::Link(link);

            self.write_trb(len - 1, trb.into());

            self.cycle = !self.cycle;
        } else if self.i >= len {
            self.i = 0;
        }

        self.i
    }

    pub fn trb_bus_addr(&self, i: usize) -> BusAddr {
        let base = self.bus_addr().raw();
        (base + (i * size_of::<TrbData>()) as u64).into()
    }

    pub fn trb_bus_addr_list(&self) -> impl Iterator<Item = BusAddr> + '_ {
        (0..self.len()).map(move |i| self.trb_bus_addr(i))
    }

    fn index_of(&self, addr: BusAddr) -> Option<usize> {
        let offset = addr.raw().checked_sub(self.bus_addr().raw())? as usize;
        (offset.is_multiple_of(TRB_SIZE) && offset / TRB_SIZE < self.len())
            .then_some(offset / TRB_SIZE)
    }

    /// The address of the TRB the controller executes after the one at `addr`, past the Link TRB (Linux
    /// `next_trb`, skipping a link).
    pub(crate) fn next_trb_addr(&self, addr: BusAddr) -> Option<BusAddr> {
        let next = self.index_of(addr)? + 1;
        let next = if (self.link && next >= self.len() - 1) || next >= self.len() {
            0
        } else {
            next
        };
        Some(self.trb_bus_addr(next))
    }

    /// Turns the TRB at `addr` into a No Op Command and keeps only its cycle bit, as Linux's `trb_to_noop` does
    /// to an aborted command. The cycle word is written last.
    pub(crate) fn command_to_noop(&mut self, addr: BusAddr) -> bool {
        let Some(index) = self.index_of(addr) else {
            return false;
        };
        let mut noop = command::Noop::default();
        if self.read_word(index, 3) & 1 != 0 {
            noop.set_cycle_bit();
        } else {
            noop.clear_cycle_bit();
        }
        self.write_trb(index, command::Allowed::Noop(noop).into());
        true
    }

    /// The ring positions of a TD, `first` to `last`, as the controller walks them (the Link TRB skipped, Linux
    /// `next_trb`). Empty if either is not on this ring or `last` is not reached within one lap.
    pub(crate) fn td_addrs(&self, first: BusAddr, last: BusAddr) -> Vec<BusAddr> {
        let mut addrs = Vec::new();
        let (Some(_), Some(_)) = (self.index_of(first), self.index_of(last)) else {
            return addrs;
        };
        let mut addr = first;
        for _ in 0..self.len() {
            addrs.push(addr);
            if addr == last {
                return addrs;
            }
            let Some(next) = self.next_trb_addr(addr) else {
                break;
            };
            addr = next;
        }
        Vec::new()
    }

    /// Whether `addr` is one of the TRBs of the TD `first..=last` (Linux `trb_in_td`).
    pub(crate) fn td_contains(&self, first: BusAddr, last: BusAddr, addr: BusAddr) -> bool {
        self.td_addrs(first, last).contains(&addr)
    }

    /// Turns every TRB of the TD `first..=last` into a Transfer No Op that keeps only its cycle bit, as Linux's
    /// `td_to_noop` with `trb_to_noop` (`xhci-ring.c:129-143`): the other fields and the chain bit are cleared, and
    /// the cycle word is written last. The controller then steps over the TD without a transfer or an event.
    pub(crate) fn td_to_noop(&mut self, first: BusAddr, last: BusAddr) {
        for addr in self.td_addrs(first, last) {
            let Some(index) = self.index_of(addr) else {
                continue;
            };
            let mut noop = transfer::Noop::default();
            if self.read_word(index, 3) & 1 != 0 {
                noop.set_cycle_bit();
            } else {
                noop.clear_cycle_bit();
            }
            self.write_trb(index, transfer::Allowed::Noop(noop).into());
        }
    }

    /// The dequeue pointer and cycle state just past the TD whose last TRB is `last`, when the controller stopped
    /// with its dequeue pointer at `hw_dequeue` (cycle state `hw_cycle`) inside that TD: Linux
    /// `xhci_move_dequeue_past_td`, which walks the ring from the hardware dequeue pointer, toggling the cycle
    /// state at each Link TRB it passes. `None` if either address is not on this ring.
    pub(crate) fn past_td(
        &self,
        hw_dequeue: BusAddr,
        hw_cycle: bool,
        last: BusAddr,
    ) -> Option<(BusAddr, bool)> {
        let mut index = self.index_of(hw_dequeue)?;
        let last = self.index_of(last)?;
        let mut cycle = hw_cycle;
        if self.link && index >= self.len() - 1 {
            // Stopped on the Link TRB itself: the controller follows it first.
            index = 0;
            cycle = !cycle;
        }
        for _ in 0..=self.len() {
            let at_last = index == last;
            let next = index + 1;
            if self.link && next >= self.len() - 1 {
                index = 0;
                cycle = !cycle;
            } else if next >= self.len() {
                index = 0;
            } else {
                index = next;
            }
            if at_last {
                return Some((self.trb_bus_addr(index), cycle));
            }
        }
        None
    }
}

pub struct SendRing<R> {
    ring: Ring,
    finished: Finished<R>,
}

impl<R> SendRing<R> {
    pub fn new(direction: DmaDirection, dma: &Kernel) -> Result<Self> {
        let ring = Ring::new(true, direction, dma)?;
        let finished = Finished::new(ring.trb_bus_addr_list());
        Ok(Self { ring, finished })
    }

    pub fn new_with_pages(pages: usize, direction: DmaDirection, dma: &Kernel) -> Result<Self> {
        let ring = Ring::new_with_pages(pages, true, direction, dma)?;
        let finished = Finished::new(ring.trb_bus_addr_list());
        Ok(Self { ring, finished })
    }

    pub fn enque_command(&mut self, trb: command::Allowed) -> BusAddr {
        let addr = self.ring.enque_command(trb);
        self.finished.clear_finished(addr);
        addr
    }

    pub fn enque_transfer(&mut self, trb: transfer::Allowed) -> BusAddr {
        let addr = self.ring.enque_transfer(trb);
        self.finished.clear_finished(addr);
        addr
    }

    pub fn enqueue_transfer_td(&mut self, trbs: &mut [transfer::Allowed]) -> Vec<BusAddr> {
        let mut addrs = Vec::with_capacity(trbs.len());
        let Some((first, rest)) = trbs.split_first_mut() else {
            return addrs;
        };

        let (first_addr, first_index, visible_first) =
            self.ring.enque_transfer_deferred_cycle(*first);
        self.finished.clear_finished(first_addr);
        addrs.push(first_addr);

        for trb in rest {
            let addr = self.ring.enque_transfer(*trb);
            self.finished.clear_finished(addr);
            addrs.push(addr);
        }

        mb();
        self.ring.set_transfer_trb(first_index, visible_first);
        addrs
    }

    pub fn take_finished_future(&self, addr: BusAddr) -> TWaiter<R> {
        self.finished.take_waiter(addr)
    }

    pub fn finished_handle(&self) -> Finished<R> {
        self.finished.clone()
    }

    pub fn get_finished(&self, addr: BusAddr) -> Option<R> {
        self.finished.get_finished(addr)
    }

    /// The completion stored at `addr`, left where it is.
    pub fn peek_finished(&self, addr: BusAddr) -> Option<R>
    where
        R: Copy,
    {
        self.finished.peek_finished(addr)
    }

    /// Takes the completion stored at `addr` only if `take` accepts it.
    pub fn take_finished_if(&self, addr: BusAddr, take: impl FnOnce(&R) -> bool) -> Option<R> {
        self.finished.take_finished_if(addr, take)
    }

    pub(crate) fn td_contains(&self, first: BusAddr, last: BusAddr, addr: BusAddr) -> bool {
        self.ring.td_contains(first, last, addr)
    }

    pub(crate) fn td_to_noop(&mut self, first: BusAddr, last: BusAddr) {
        self.ring.td_to_noop(first, last);
    }

    pub(crate) fn past_td(
        &self,
        hw_dequeue: BusAddr,
        hw_cycle: bool,
        last: BusAddr,
    ) -> Option<(BusAddr, bool)> {
        self.ring.past_td(hw_dequeue, hw_cycle, last)
    }

    pub(crate) fn read_trb_word(&self, addr: BusAddr, word: usize) -> Option<u32> {
        let index = self.ring.index_of(addr)?;
        Some(self.ring.read_word(index, word))
    }

    pub fn register_cx(&self, addr: BusAddr, cx: &mut core::task::Context<'_>) {
        self.finished.register_cx(addr, cx);
    }

    pub fn bus_addr(&self) -> BusAddr {
        self.ring.bus_addr()
    }

    pub fn usable_capacity(&self) -> usize {
        self.ring.len().saturating_sub(1)
    }

    pub fn cycle(&self) -> bool {
        self.ring.cycle
    }

    pub(crate) fn next_trb_addr(&self, addr: BusAddr) -> Option<BusAddr> {
        self.ring.next_trb_addr(addr)
    }

    pub(crate) fn command_to_noop(&mut self, addr: BusAddr) -> bool {
        self.ring.command_to_noop(addr)
    }

    pub fn enqueue_pointer(&self) -> (BusAddr, bool) {
        (self.ring.trb_bus_addr(self.ring.i), self.ring.cycle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::kmod::xhci::endpoint::tests::test_kernel;

    /// A transfer ring of 8 positions: 7 TRBs and the Link TRB, producer cycle state 1.
    fn ring8() -> Ring {
        Ring::new_with_len(8, true, DmaDirection::Bidirectional, &test_kernel()).unwrap()
    }

    fn chained_normal() -> transfer::Allowed {
        transfer::Allowed::Normal(
            *transfer::Normal::new()
                .set_data_buffer_pointer(0x1234_5000)
                .set_trb_transfer_length(64)
                .set_chain_bit()
                .set_interrupt_on_completion(),
        )
    }

    fn addr(ring: &Ring, index: usize) -> BusAddr {
        ring.trb_bus_addr(index)
    }

    #[test]
    fn the_dequeue_pointer_moves_to_the_trb_after_the_tds_last() {
        let ring = ring8();
        // Stopped on the TD's second TRB (2), TD is 1..=3: the new dequeue is TRB 4, same cycle state.
        assert_eq!(
            ring.past_td(addr(&ring, 2), true, addr(&ring, 3)),
            Some((addr(&ring, 4), true))
        );
        // Stopped on the first TRB of a one-TRB TD.
        assert_eq!(
            ring.past_td(addr(&ring, 1), false, addr(&ring, 1)),
            Some((addr(&ring, 2), false))
        );
    }

    #[test]
    fn the_dequeue_pointer_wraps_over_the_link_trb_and_toggles_the_cycle_state() {
        let ring = ring8();
        // The last TRB is the one before the Link TRB (6): the next is TRB 0 of the next lap.
        assert_eq!(
            ring.past_td(addr(&ring, 5), true, addr(&ring, 6)),
            Some((addr(&ring, 0), false))
        );
        // A TD across the Link TRB (5, 6, 0, 1), stopped before the wrap: the cycle state toggles once, passing it.
        assert_eq!(
            ring.past_td(addr(&ring, 6), true, addr(&ring, 1)),
            Some((addr(&ring, 2), false))
        );
        // Stopped after the wrap, inside the same TD, the hardware cycle state is already the new lap's.
        assert_eq!(
            ring.past_td(addr(&ring, 0), false, addr(&ring, 1)),
            Some((addr(&ring, 2), false))
        );
        // Stopped on the Link TRB itself: the controller follows it first.
        assert_eq!(
            ring.past_td(addr(&ring, 7), true, addr(&ring, 1)),
            Some((addr(&ring, 2), false))
        );
    }

    #[test]
    fn a_dequeue_pointer_off_the_ring_has_no_new_position() {
        let ring = ring8();
        let off = BusAddr::from(ring.bus_addr().raw() + 8 * TRB_SIZE as u64);
        assert_eq!(ring.past_td(off, true, addr(&ring, 1)), None);
        assert_eq!(ring.past_td(addr(&ring, 1), true, off), None);
    }

    #[test]
    fn a_td_spans_the_trbs_from_first_to_last_skipping_the_link_trb() {
        let ring = ring8();
        let td = ring.td_addrs(addr(&ring, 5), addr(&ring, 1));
        assert_eq!(
            td,
            [5, 6, 0, 1].map(|index| addr(&ring, index)).to_vec()
        );
        assert!(ring.td_contains(addr(&ring, 5), addr(&ring, 1), addr(&ring, 0)));
        assert!(!ring.td_contains(addr(&ring, 5), addr(&ring, 1), addr(&ring, 2)));
        assert!(!ring.td_contains(addr(&ring, 5), addr(&ring, 1), addr(&ring, 7)));
    }

    #[test]
    fn a_td_turned_into_no_ops_keeps_its_cycle_bits_and_loses_its_chain_and_fields() {
        let mut ring = ring8();
        // Four TRBs of other TDs, then the TD's three, all on the first lap: cycle bit 1.
        for _ in 0..4 {
            ring.enque_transfer(chained_normal());
        }
        let first = ring.enque_transfer(chained_normal());
        ring.enque_transfer(chained_normal());
        let last = ring.enque_transfer(chained_normal());
        // The next lap starts after the Link TRB: cycle bit 0.
        let second_lap = ring.enque_transfer(chained_normal());

        ring.td_to_noop(first, last);
        ring.td_to_noop(second_lap, second_lap);

        let noop_type = (transfer::Noop::default().into_raw()[3] >> 10) & 0x3f;
        assert_eq!(noop_type, 8);
        for (index, cycle) in [(4, 1), (5, 1), (6, 1), (0, 0)] {
            let word3 = ring.read_word(index, 3);
            assert_eq!(word3 & 1, cycle, "cycle bit of TRB {index}");
            assert_eq!((word3 >> 10) & 0x3f, 8, "TRB {index} is a Transfer No Op");
            assert_eq!(word3 & (1 << 4), 0, "chain bit of TRB {index}");
            assert_eq!(word3 & (1 << 5), 0, "IOC of TRB {index}");
            for word in 0..3 {
                assert_eq!(ring.read_word(index, word), 0, "word {word} of TRB {index}");
            }
        }
        // The TRBs of other TDs are untouched.
        let other = ring.read_word(3, 3);
        assert_eq!((other >> 10) & 0x3f, 1, "a Normal TRB");
        assert_ne!(other & (1 << 4), 0);
    }
}
