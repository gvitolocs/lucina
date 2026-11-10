// Some of the functions in ring buffer is marked as #[must_use]. It notes that
// these functions may have side effects, and it's implemented by [RFC 1940].
// [RFC 1940]: https://github.com/rust-lang/rust/issues/43302

use core::cmp;
use managed::ManagedSlice;

use crate::storage::Resettable;

use super::{Empty, Full};

/// A ring buffer.
///
/// This ring buffer implementation provides many ways to interact with it:
///
///   * Enqueueing or dequeueing one element from corresponding side of the buffer;
///   * Enqueueing or dequeueing a slice of elements from corresponding side of the buffer;
///   * Accessing allocated and unallocated areas directly.
///
/// It is also zero-copy; all methods provide references into the buffer's storage.
/// Note that all references are mutable; it is considered more important to allow
/// in-place processing than to protect from accidental mutation.
///
/// This implementation is suitable for both simple uses such as a FIFO queue
/// of UDP packets, and advanced ones such as a TCP reassembly buffer.
///
/// LucinaOS: the storage can also be a list of pages ([`RingBuffer::new_paged`]), so that a large buffer needs no
/// physically contiguous allocation. Every slice this buffer hands out then ends at a page end as well as at the end of
/// the storage, which the slice methods already allow ("the largest contiguous slice"), and the methods that copy
/// (`enqueue_slice`, `dequeue_slice`, `write_unallocated`, `read_allocated`) go round as many pieces as they need.
#[derive(Debug)]
pub struct RingBuffer<'a, T: 'a> {
    storage: Storage<'a, T>,
    read_at: usize,
    length: usize,
}

/// Where a ring buffer keeps its elements: one slice, or (LucinaOS) pages of `page` elements each, the last one shorter
/// when the capacity is not a multiple of it.
#[derive(Debug)]
enum Storage<'a, T: 'a> {
    Slice(ManagedSlice<'a, T>),
    #[cfg(feature = "alloc")]
    Paged {
        pages: alloc::vec::Vec<alloc::vec::Vec<T>>,
        page: usize,
        len: usize,
    },
}

impl<'a, T: 'a> Storage<'a, T> {
    fn len(&self) -> usize {
        match self {
            Storage::Slice(slice) => slice.len(),
            #[cfg(feature = "alloc")]
            Storage::Paged { len, .. } => *len,
        }
    }

    /// The elements from `at` to the end of the piece that holds `at`; empty when `at` is past the end.
    fn piece(&self, at: usize) -> &[T] {
        match self {
            Storage::Slice(slice) => slice.get(at..).unwrap_or(&[]),
            #[cfg(feature = "alloc")]
            Storage::Paged { pages, page, len } => {
                // `piece_at` (Verus-proved) keeps the piece inside its page and the storage.
                let (index, offset, n) = super::paged_math::piece_at(at, *page, *len);
                match pages.get(index) {
                    Some(p) => p.get(offset..offset + n).unwrap_or(&[]),
                    None => &[],
                }
            }
        }
    }

    /// As [`Storage::piece`], writable.
    fn piece_mut(&mut self, at: usize) -> &mut [T] {
        match self {
            Storage::Slice(slice) => slice.get_mut(at..).unwrap_or(&mut []),
            #[cfg(feature = "alloc")]
            Storage::Paged { pages, page, len } => {
                let (index, offset, n) = super::paged_math::piece_at(at, *page, *len);
                match pages.get_mut(index) {
                    Some(p) => p.get_mut(offset..offset + n).unwrap_or(&mut []),
                    None => &mut [],
                }
            }
        }
    }

    /// Up to `size` elements from index `at`, no further than the end of the piece that holds `at`.
    fn slice(&self, at: usize, size: usize) -> &[T] {
        let piece = self.piece(at);
        let size = cmp::min(size, piece.len());
        &piece[..size]
    }

    /// As [`Storage::slice`], writable.
    fn slice_mut(&mut self, at: usize, size: usize) -> &mut [T] {
        let piece = self.piece_mut(at);
        let size = cmp::min(size, piece.len());
        &mut piece[..size]
    }

    /// Every element, piece by piece.
    fn for_each_mut(&mut self, mut f: impl FnMut(&mut T)) {
        match self {
            Storage::Slice(slice) => slice.iter_mut().for_each(f),
            #[cfg(feature = "alloc")]
            Storage::Paged { pages, .. } => pages.iter_mut().flatten().for_each(&mut f),
        }
    }
}

impl<'a, T: 'a> core::ops::Index<usize> for Storage<'a, T> {
    type Output = T;

    fn index(&self, at: usize) -> &T {
        &self.piece(at)[0]
    }
}

/// The tests look at a one-slice storage whole.
#[cfg(test)]
impl<'a, T: 'a> core::ops::Index<core::ops::RangeFull> for Storage<'a, T> {
    type Output = [T];

    fn index(&self, _: core::ops::RangeFull) -> &[T] {
        match self {
            Storage::Slice(slice) => slice,
            #[cfg(feature = "alloc")]
            Storage::Paged { .. } => panic!("a paged storage is not one slice"),
        }
    }
}

impl<'a, T: 'a> core::ops::IndexMut<usize> for Storage<'a, T> {
    fn index_mut(&mut self, at: usize) -> &mut T {
        &mut self.piece_mut(at)[0]
    }
}

impl<'a, T: 'a> RingBuffer<'a, T> {
    /// Create a ring buffer with the given storage.
    ///
    /// During creation, every element in `storage` is reset.
    pub fn new<S>(storage: S) -> RingBuffer<'a, T>
    where
        S: Into<ManagedSlice<'a, T>>,
    {
        RingBuffer {
            storage: Storage::Slice(storage.into()),
            read_at: 0,
            length: 0,
        }
    }

    /// LucinaOS: create a ring buffer of `capacity` elements kept in pages of `page` elements, each its own allocation,
    /// so that no allocation is larger than a page. It fails, instead of aborting, when an allocation fails.
    ///
    /// # Panics
    /// Panics if `page` is zero.
    #[cfg(feature = "alloc")]
    pub fn new_paged(
        capacity: usize,
        page: usize,
    ) -> Result<RingBuffer<'a, T>, alloc::collections::TryReserveError>
    where
        T: Clone + Default,
    {
        assert!(page > 0, "a ring buffer page holds at least one element");
        let count = capacity.div_ceil(page);
        let mut pages = alloc::vec::Vec::new();
        pages.try_reserve_exact(count)?;
        for i in 0..count {
            let size = cmp::min(page, capacity - i * page);
            let mut p = alloc::vec::Vec::new();
            p.try_reserve_exact(size)?;
            p.resize(size, T::default());
            pages.push(p);
        }
        Ok(RingBuffer {
            storage: Storage::Paged {
                pages,
                page,
                len: capacity,
            },
            read_at: 0,
            length: 0,
        })
    }

    /// The largest number of elements that are contiguous in the storage from index `at`: to the end of the storage,
    /// and to the end of the page that holds `at` when the storage is paged.
    fn contiguous_at(&self, at: usize) -> usize {
        self.storage.piece(at).len()
    }



    /// Clear the ring buffer.
    pub fn clear(&mut self) {
        self.read_at = 0;
        self.length = 0;
    }

    /// Return the maximum number of elements in the ring buffer.
    pub fn capacity(&self) -> usize {
        self.storage.len()
    }

    /// Clear the ring buffer, and reset every element.
    pub fn reset(&mut self)
    where
        T: Resettable,
    {
        self.clear();
        self.storage.for_each_mut(|elem| elem.reset());
    }

    /// Return the current number of elements in the ring buffer.
    pub fn len(&self) -> usize {
        self.length
    }

    /// Return the number of elements that can be added to the ring buffer.
    pub fn window(&self) -> usize {
        self.capacity() - self.len()
    }

    /// Return the largest number of elements that can be added to the buffer
    /// without wrapping around (i.e. in a single `enqueue_many` call).
    pub fn contiguous_window(&self) -> usize {
        cmp::min(self.window(), self.contiguous_at(self.get_idx(self.length)))
    }

    /// Query whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Query whether the buffer is full.
    pub fn is_full(&self) -> bool {
        self.window() == 0
    }

    /// Shorthand for `(self.read + idx) % self.capacity()` with an
    /// additional check to ensure that the capacity is not zero.
    fn get_idx(&self, idx: usize) -> usize {
        let len = self.capacity();
        if len > 0 {
            (self.read_at + idx) % len
        } else {
            0
        }
    }

    /// Shorthand for `(self.read + idx) % self.capacity()` with no
    /// additional checks to ensure the capacity is not zero.
    fn get_idx_unchecked(&self, idx: usize) -> usize {
        (self.read_at + idx) % self.capacity()
    }
}

/// This is the "discrete" ring buffer interface: it operates with single elements,
/// and boundary conditions (empty/full) are errors.
impl<'a, T: 'a> RingBuffer<'a, T> {
    /// Call `f` with a single buffer element, and enqueue the element if `f`
    /// returns successfully, or return `Err(Full)` if the buffer is full.
    pub fn enqueue_one_with<'b, R, E, F>(&'b mut self, f: F) -> Result<Result<R, E>, Full>
    where
        F: FnOnce(&'b mut T) -> Result<R, E>,
    {
        if self.is_full() {
            return Err(Full);
        }

        let index = self.get_idx_unchecked(self.length);
        let res = f(&mut self.storage[index]);
        if res.is_ok() {
            self.length += 1;
        }
        Ok(res)
    }

    /// Enqueue a single element into the buffer, and return a reference to it,
    /// or return `Err(Full)` if the buffer is full.
    ///
    /// This function is a shortcut for `ring_buf.enqueue_one_with(Ok)`.
    pub fn enqueue_one(&mut self) -> Result<&mut T, Full> {
        self.enqueue_one_with(Ok)?
    }

    /// Call `f` with a single buffer element, and dequeue the element if `f`
    /// returns successfully, or return `Err(Empty)` if the buffer is empty.
    pub fn dequeue_one_with<'b, R, E, F>(&'b mut self, f: F) -> Result<Result<R, E>, Empty>
    where
        F: FnOnce(&'b mut T) -> Result<R, E>,
    {
        if self.is_empty() {
            return Err(Empty);
        }

        let next_at = self.get_idx_unchecked(1);
        let res = f(&mut self.storage[self.read_at]);

        if res.is_ok() {
            self.length -= 1;
            self.read_at = next_at;
        }
        Ok(res)
    }

    /// Dequeue an element from the buffer, and return a reference to it,
    /// or return `Err(Empty)` if the buffer is empty.
    ///
    /// This function is a shortcut for `ring_buf.dequeue_one_with(Ok)`.
    pub fn dequeue_one(&mut self) -> Result<&mut T, Empty> {
        self.dequeue_one_with(Ok)?
    }
}

/// This is the "continuous" ring buffer interface: it operates with element slices,
/// and boundary conditions (empty/full) simply result in empty slices.
impl<'a, T: 'a> RingBuffer<'a, T> {
    /// Call `f` with the largest contiguous slice of unallocated buffer elements,
    /// and enqueue the amount of elements returned by `f`.
    ///
    /// # Panics
    /// This function panics if the amount of elements returned by `f` is larger
    /// than the size of the slice passed into it.
    pub fn enqueue_many_with<'b, R, F>(&'b mut self, f: F) -> (usize, R)
    where
        F: FnOnce(&'b mut [T]) -> (usize, R),
    {
        if self.length == 0 {
            // Ring is currently empty. Reset `read_at` to optimize
            // for contiguous space.
            self.read_at = 0;
        }

        let write_at = self.get_idx(self.length);
        let max_size = self.contiguous_window();
        let (size, result) = f(self.storage.slice_mut(write_at, max_size));
        assert!(size <= max_size);
        self.length += size;
        (size, result)
    }

    /// Enqueue a slice of elements up to the given size into the buffer,
    /// and return a reference to them.
    ///
    /// This function may return a slice smaller than the given size
    /// if the free space in the buffer is not contiguous.
    #[must_use]
    pub fn enqueue_many(&mut self, size: usize) -> &mut [T] {
        self.enqueue_many_with(|buf| {
            let size = cmp::min(size, buf.len());
            (size, &mut buf[..size])
        })
        .1
    }

    /// Enqueue as many elements from the given slice into the buffer as possible,
    /// and return the amount of elements that could fit.
    #[must_use]
    pub fn enqueue_slice(&mut self, data: &[T]) -> usize
    where
        T: Copy,
    {
        // Upstream copies in two goes (up to the end of the storage, then from its start); a paged storage needs one go
        // per page, so this goes on until a go copies nothing. On a one-slice storage that is the same two goes.
        let mut done = 0;
        loop {
            let (size, ()) = self.enqueue_many_with(|buf| {
                let size = cmp::min(buf.len(), data.len() - done);
                buf[..size].copy_from_slice(&data[done..done + size]);
                (size, ())
            });
            if size == 0 {
                break done;
            }
            done += size;
        }
    }

    /// Call `f` with the largest contiguous slice of allocated buffer elements,
    /// and dequeue the amount of elements returned by `f`.
    ///
    /// # Panics
    /// This function panics if the amount of elements returned by `f` is larger
    /// than the size of the slice passed into it.
    pub fn dequeue_many_with<'b, R, F>(&'b mut self, f: F) -> (usize, R)
    where
        F: FnOnce(&'b mut [T]) -> (usize, R),
    {
        let capacity = self.capacity();
        let read_at = self.read_at;
        let max_size = cmp::min(self.len(), self.contiguous_at(read_at));
        let (size, result) = f(self.storage.slice_mut(read_at, max_size));
        assert!(size <= max_size);
        self.read_at = if capacity > 0 {
            (self.read_at + size) % capacity
        } else {
            0
        };
        self.length -= size;
        (size, result)
    }

    /// Dequeue a slice of elements up to the given size from the buffer,
    /// and return a reference to them.
    ///
    /// This function may return a slice smaller than the given size
    /// if the allocated space in the buffer is not contiguous.
    #[must_use]
    pub fn dequeue_many(&mut self, size: usize) -> &mut [T] {
        self.dequeue_many_with(|buf| {
            let size = cmp::min(size, buf.len());
            (size, &mut buf[..size])
        })
        .1
    }

    /// Dequeue as many elements from the buffer into the given slice as possible,
    /// and return the amount of elements that could fit.
    #[must_use]
    pub fn dequeue_slice(&mut self, data: &mut [T]) -> usize
    where
        T: Copy,
    {
        // As in `enqueue_slice`: one go per piece, until a go copies nothing.
        let mut done = 0;
        loop {
            let (size, ()) = self.dequeue_many_with(|buf| {
                let size = cmp::min(buf.len(), data.len() - done);
                data[done..done + size].copy_from_slice(&buf[..size]);
                (size, ())
            });
            if size == 0 {
                break done;
            }
            done += size;
        }
    }
}

/// This is the "random access" ring buffer interface: it operates with element slices,
/// and allows to access elements of the buffer that are not adjacent to its head or tail.
impl<'a, T: 'a> RingBuffer<'a, T> {
    /// Return the largest contiguous slice of unallocated buffer elements starting
    /// at the given offset past the last allocated element, and up to the given size.
    #[must_use]
    pub fn get_unallocated(&mut self, offset: usize, mut size: usize) -> &mut [T] {
        let start_at = self.get_idx(self.length + offset);
        // We can't access past the end of unallocated data.
        if offset > self.window() {
            return &mut [];
        }
        // We can't enqueue more than there is free space.
        let clamped_window = self.window() - offset;
        if size > clamped_window {
            size = clamped_window
        }
        // We can't contiguously enqueue past the end of the storage (or of a page).
        self.storage.slice_mut(start_at, size)
    }

    /// Write as many elements from the given slice into unallocated buffer elements
    /// starting at the given offset past the last allocated element, and return
    /// the amount written.
    #[must_use]
    pub fn write_unallocated<P>(&mut self, offset: usize, data: P) -> usize
    where
        T: Copy,
        P: super::SliceLike<Item = T>,
    {
        // As in `enqueue_slice`: one go per piece, until a go copies nothing.
        let mut done = 0;
        loop {
            let rest = data.index(done..data.len());
            let slice = self.get_unallocated(offset + done, rest.len());
            let slice_len = slice.len();
            if slice_len == 0 {
                break done;
            }
            rest.index(0..slice_len).copy_to_slice(slice);
            done += slice_len;
        }
    }

    /// Enqueue the given number of unallocated buffer elements.
    ///
    /// # Panics
    /// Panics if the number of elements given exceeds the number of unallocated elements.
    pub fn enqueue_unallocated(&mut self, count: usize) {
        assert!(count <= self.window());
        self.length += count;
    }

    /// Return the largest contiguous slice of allocated buffer elements starting
    /// at the given offset past the first allocated element, and up to the given size.
    #[must_use]
    pub fn get_allocated(&self, offset: usize, mut size: usize) -> &[T] {
        let start_at = self.get_idx(offset);
        // We can't read past the end of the allocated data.
        if offset > self.length {
            return &mut [];
        }
        // We can't read more than we have allocated.
        let clamped_length = self.length - offset;
        if size > clamped_length {
            size = clamped_length
        }
        // We can't contiguously dequeue past the end of the storage (or of a page).
        self.storage.slice(start_at, size)
    }

    /// Read as many elements from allocated buffer elements into the given slice
    /// starting at the given offset past the first allocated element, and return
    /// the amount read.
    #[must_use]
    pub fn read_allocated(&mut self, offset: usize, data: &mut [T]) -> usize
    where
        T: Copy,
    {
        // As in `enqueue_slice`: one go per piece, until a go copies nothing.
        let mut done = 0;
        loop {
            let slice = self.get_allocated(offset + done, data.len() - done);
            let slice_len = slice.len();
            if slice_len == 0 {
                break done;
            }
            data[done..done + slice_len].copy_from_slice(slice);
            done += slice_len;
        }
    }

    /// Dequeue the given number of allocated buffer elements.
    ///
    /// # Panics
    /// Panics if the number of elements given exceeds the number of allocated elements.
    pub fn dequeue_allocated(&mut self, count: usize) {
        assert!(count <= self.len());
        self.length -= count;
        self.read_at = self.get_idx(count);
    }
}

impl<'a, T: 'a> From<ManagedSlice<'a, T>> for RingBuffer<'a, T> {
    fn from(slice: ManagedSlice<'a, T>) -> RingBuffer<'a, T> {
        RingBuffer::new(slice)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_buffer_length_changes() {
        let mut ring = RingBuffer::new(vec![0; 2]);
        assert!(ring.is_empty());
        assert!(!ring.is_full());
        assert_eq!(ring.len(), 0);
        assert_eq!(ring.capacity(), 2);
        assert_eq!(ring.window(), 2);

        ring.length = 1;
        assert!(!ring.is_empty());
        assert!(!ring.is_full());
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.capacity(), 2);
        assert_eq!(ring.window(), 1);

        ring.length = 2;
        assert!(!ring.is_empty());
        assert!(ring.is_full());
        assert_eq!(ring.len(), 2);
        assert_eq!(ring.capacity(), 2);
        assert_eq!(ring.window(), 0);
    }

    #[test]
    fn test_buffer_enqueue_dequeue_one_with() {
        let mut ring = RingBuffer::new(vec![0; 5]);
        assert_eq!(
            ring.dequeue_one_with(|_| -> Result::<(), ()> { unreachable!() }),
            Err(Empty)
        );

        ring.enqueue_one_with(Ok::<_, ()>).unwrap().unwrap();
        assert!(!ring.is_empty());
        assert!(!ring.is_full());

        for i in 1..5 {
            ring.enqueue_one_with(|e| Ok::<_, ()>(*e = i))
                .unwrap()
                .unwrap();
            assert!(!ring.is_empty());
        }
        assert!(ring.is_full());
        assert_eq!(
            ring.enqueue_one_with(|_| -> Result::<(), ()> { unreachable!() }),
            Err(Full)
        );

        for i in 0..5 {
            assert_eq!(
                ring.dequeue_one_with(|e| Ok::<_, ()>(*e)).unwrap().unwrap(),
                i
            );
            assert!(!ring.is_full());
        }
        assert_eq!(
            ring.dequeue_one_with(|_| -> Result::<(), ()> { unreachable!() }),
            Err(Empty)
        );
        assert!(ring.is_empty());
    }

    #[test]
    fn test_buffer_enqueue_dequeue_one() {
        let mut ring = RingBuffer::new(vec![0; 5]);
        assert_eq!(ring.dequeue_one(), Err(Empty));

        ring.enqueue_one().unwrap();
        assert!(!ring.is_empty());
        assert!(!ring.is_full());

        for i in 1..5 {
            *ring.enqueue_one().unwrap() = i;
            assert!(!ring.is_empty());
        }
        assert!(ring.is_full());
        assert_eq!(ring.enqueue_one(), Err(Full));

        for i in 0..5 {
            assert_eq!(*ring.dequeue_one().unwrap(), i);
            assert!(!ring.is_full());
        }
        assert_eq!(ring.dequeue_one(), Err(Empty));
        assert!(ring.is_empty());
    }

    #[test]
    fn test_buffer_enqueue_many_with() {
        let mut ring = RingBuffer::new(vec![b'.'; 12]);

        assert_eq!(
            ring.enqueue_many_with(|buf| {
                assert_eq!(buf.len(), 12);
                buf[0..2].copy_from_slice(b"ab");
                (2, true)
            }),
            (2, true)
        );
        assert_eq!(ring.len(), 2);
        assert_eq!(&ring.storage[..], b"ab..........");

        ring.enqueue_many_with(|buf| {
            assert_eq!(buf.len(), 12 - 2);
            buf[0..4].copy_from_slice(b"cdXX");
            (2, ())
        });
        assert_eq!(ring.len(), 4);
        assert_eq!(&ring.storage[..], b"abcdXX......");

        ring.enqueue_many_with(|buf| {
            assert_eq!(buf.len(), 12 - 4);
            buf[0..4].copy_from_slice(b"efgh");
            (4, ())
        });
        assert_eq!(ring.len(), 8);
        assert_eq!(&ring.storage[..], b"abcdefgh....");

        for _ in 0..4 {
            *ring.dequeue_one().unwrap() = b'.';
        }
        assert_eq!(ring.len(), 4);
        assert_eq!(&ring.storage[..], b"....efgh....");

        ring.enqueue_many_with(|buf| {
            assert_eq!(buf.len(), 12 - 8);
            buf[0..4].copy_from_slice(b"ijkl");
            (4, ())
        });
        assert_eq!(ring.len(), 8);
        assert_eq!(&ring.storage[..], b"....efghijkl");

        ring.enqueue_many_with(|buf| {
            assert_eq!(buf.len(), 4);
            buf[0..4].copy_from_slice(b"abcd");
            (4, ())
        });
        assert_eq!(ring.len(), 12);
        assert_eq!(&ring.storage[..], b"abcdefghijkl");

        for _ in 0..4 {
            *ring.dequeue_one().unwrap() = b'.';
        }
        assert_eq!(ring.len(), 8);
        assert_eq!(&ring.storage[..], b"abcd....ijkl");
    }

    #[test]
    fn test_buffer_enqueue_many() {
        let mut ring = RingBuffer::new(vec![b'.'; 12]);

        ring.enqueue_many(8).copy_from_slice(b"abcdefgh");
        assert_eq!(ring.len(), 8);
        assert_eq!(&ring.storage[..], b"abcdefgh....");

        ring.enqueue_many(8).copy_from_slice(b"ijkl");
        assert_eq!(ring.len(), 12);
        assert_eq!(&ring.storage[..], b"abcdefghijkl");
    }

    #[test]
    fn test_buffer_enqueue_slice() {
        let mut ring = RingBuffer::new(vec![b'.'; 12]);

        assert_eq!(ring.enqueue_slice(b"abcdefgh"), 8);
        assert_eq!(ring.len(), 8);
        assert_eq!(&ring.storage[..], b"abcdefgh....");

        for _ in 0..4 {
            *ring.dequeue_one().unwrap() = b'.';
        }
        assert_eq!(ring.len(), 4);
        assert_eq!(&ring.storage[..], b"....efgh....");

        assert_eq!(ring.enqueue_slice(b"ijklabcd"), 8);
        assert_eq!(ring.len(), 12);
        assert_eq!(&ring.storage[..], b"abcdefghijkl");
    }

    #[test]
    fn test_buffer_dequeue_many_with() {
        let mut ring = RingBuffer::new(vec![b'.'; 12]);

        assert_eq!(ring.enqueue_slice(b"abcdefghijkl"), 12);

        assert_eq!(
            ring.dequeue_many_with(|buf| {
                assert_eq!(buf.len(), 12);
                assert_eq!(buf, b"abcdefghijkl");
                buf[..4].copy_from_slice(b"....");
                (4, true)
            }),
            (4, true)
        );
        assert_eq!(ring.len(), 8);
        assert_eq!(&ring.storage[..], b"....efghijkl");

        ring.dequeue_many_with(|buf| {
            assert_eq!(buf, b"efghijkl");
            buf[..4].copy_from_slice(b"....");
            (4, ())
        });
        assert_eq!(ring.len(), 4);
        assert_eq!(&ring.storage[..], b"........ijkl");

        assert_eq!(ring.enqueue_slice(b"abcd"), 4);
        assert_eq!(ring.len(), 8);

        ring.dequeue_many_with(|buf| {
            assert_eq!(buf, b"ijkl");
            buf[..4].copy_from_slice(b"....");
            (4, ())
        });
        ring.dequeue_many_with(|buf| {
            assert_eq!(buf, b"abcd");
            buf[..4].copy_from_slice(b"....");
            (4, ())
        });
        assert_eq!(ring.len(), 0);
        assert_eq!(&ring.storage[..], b"............");
    }

    #[test]
    fn test_buffer_dequeue_many() {
        let mut ring = RingBuffer::new(vec![b'.'; 12]);

        assert_eq!(ring.enqueue_slice(b"abcdefghijkl"), 12);

        {
            let buf = ring.dequeue_many(8);
            assert_eq!(buf, b"abcdefgh");
            buf.copy_from_slice(b"........");
        }
        assert_eq!(ring.len(), 4);
        assert_eq!(&ring.storage[..], b"........ijkl");

        {
            let buf = ring.dequeue_many(8);
            assert_eq!(buf, b"ijkl");
            buf.copy_from_slice(b"....");
        }
        assert_eq!(ring.len(), 0);
        assert_eq!(&ring.storage[..], b"............");
    }

    #[test]
    fn test_buffer_dequeue_slice() {
        let mut ring = RingBuffer::new(vec![b'.'; 12]);

        assert_eq!(ring.enqueue_slice(b"abcdefghijkl"), 12);

        {
            let mut buf = [0; 8];
            assert_eq!(ring.dequeue_slice(&mut buf[..]), 8);
            assert_eq!(&buf[..], b"abcdefgh");
            assert_eq!(ring.len(), 4);
        }

        assert_eq!(ring.enqueue_slice(b"abcd"), 4);

        {
            let mut buf = [0; 8];
            assert_eq!(ring.dequeue_slice(&mut buf[..]), 8);
            assert_eq!(&buf[..], b"ijklabcd");
            assert_eq!(ring.len(), 0);
        }
    }

    #[test]
    fn test_buffer_get_unallocated() {
        let mut ring = RingBuffer::new(vec![b'.'; 12]);

        assert_eq!(ring.get_unallocated(16, 4), b"");

        {
            let buf = ring.get_unallocated(0, 4);
            buf.copy_from_slice(b"abcd");
        }
        assert_eq!(&ring.storage[..], b"abcd........");

        let buf_enqueued = ring.enqueue_many(4);
        assert_eq!(buf_enqueued.len(), 4);
        assert_eq!(ring.len(), 4);

        {
            let buf = ring.get_unallocated(4, 8);
            buf.copy_from_slice(b"ijkl");
        }
        assert_eq!(&ring.storage[..], b"abcd....ijkl");

        ring.enqueue_many(8).copy_from_slice(b"EFGHIJKL");
        ring.dequeue_many(4).copy_from_slice(b"abcd");
        assert_eq!(ring.len(), 8);
        assert_eq!(&ring.storage[..], b"abcdEFGHIJKL");

        {
            let buf = ring.get_unallocated(0, 8);
            buf.copy_from_slice(b"ABCD");
        }
        assert_eq!(&ring.storage[..], b"ABCDEFGHIJKL");
    }

    #[test]
    fn test_buffer_write_unallocated() {
        let mut ring = RingBuffer::new(vec![b'.'; 12]);
        ring.enqueue_many(6).copy_from_slice(b"abcdef");
        ring.dequeue_many(6).copy_from_slice(b"ABCDEF");

        assert_eq!(ring.write_unallocated(0, &b"ghi"[..]), 3);
        assert_eq!(ring.get_unallocated(0, 3), b"ghi");

        assert_eq!(ring.write_unallocated(3, &b"jklmno"[..]), 6);
        assert_eq!(ring.get_unallocated(3, 3), b"jkl");

        assert_eq!(ring.write_unallocated(9, &b"pqrstu"[..]), 3);
        assert_eq!(ring.get_unallocated(9, 3), b"pqr");
    }

    #[test]
    fn test_buffer_get_allocated() {
        let mut ring = RingBuffer::new(vec![b'.'; 12]);

        assert_eq!(ring.get_allocated(16, 4), b"");
        assert_eq!(ring.get_allocated(0, 4), b"");

        let len_enqueued = ring.enqueue_slice(b"abcd");
        assert_eq!(ring.get_allocated(0, 8), b"abcd");
        assert_eq!(len_enqueued, 4);

        let len_enqueued = ring.enqueue_slice(b"efghijkl");
        ring.dequeue_many(4).copy_from_slice(b"....");
        assert_eq!(ring.get_allocated(4, 8), b"ijkl");
        assert_eq!(len_enqueued, 8);

        let len_enqueued = ring.enqueue_slice(b"abcd");
        assert_eq!(ring.get_allocated(4, 8), b"ijkl");
        assert_eq!(len_enqueued, 4);
    }

    #[test]
    fn test_buffer_read_allocated() {
        let mut ring = RingBuffer::new(vec![b'.'; 12]);
        ring.enqueue_many(12).copy_from_slice(b"abcdefghijkl");

        let mut data = [0; 6];
        assert_eq!(ring.read_allocated(0, &mut data[..]), 6);
        assert_eq!(&data[..], b"abcdef");

        ring.dequeue_many(6).copy_from_slice(b"ABCDEF");
        ring.enqueue_many(3).copy_from_slice(b"mno");

        let mut data = [0; 6];
        assert_eq!(ring.read_allocated(3, &mut data[..]), 6);
        assert_eq!(&data[..], b"jklmno");

        let mut data = [0; 6];
        assert_eq!(ring.read_allocated(6, &mut data[..]), 3);
        assert_eq!(&data[..], b"mno\x00\x00\x00");
    }

    #[test]
    fn test_buffer_with_no_capacity() {
        let mut no_capacity: RingBuffer<u8> = RingBuffer::new(vec![]);

        // Call all functions that calculate the remainder against rx_buffer.capacity()
        // with a backing storage with a length of 0.
        assert_eq!(no_capacity.get_unallocated(0, 0), &[]);
        assert_eq!(no_capacity.get_allocated(0, 0), &[]);
        no_capacity.dequeue_allocated(0);
        assert_eq!(no_capacity.enqueue_many(0), &[]);
        assert_eq!(no_capacity.enqueue_one(), Err(Full));
        assert_eq!(no_capacity.contiguous_window(), 0);
    }

    /// Use the buffer a bit. Then empty it and put in an item of
    /// maximum size. By detecting a length of 0, the implementation
    /// can reset the current buffer position.
    #[test]
    fn test_buffer_write_wholly() {
        let mut ring = RingBuffer::new(vec![b'.'; 8]);
        ring.enqueue_many(2).copy_from_slice(b"ab");
        ring.enqueue_many(2).copy_from_slice(b"cd");
        assert_eq!(ring.len(), 4);
        let buf_dequeued = ring.dequeue_many(4);
        assert_eq!(buf_dequeued, b"abcd");
        assert_eq!(ring.len(), 0);

        let large = ring.enqueue_many(8);
        assert_eq!(large.len(), 8);
    }

    // LucinaOS: paged storage (`new_paged`).

    /// A small deterministic generator for the model test (no `rand` in the vendored copy).
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as usize
        }

        fn below(&mut self, n: usize) -> usize {
            if n == 0 { 0 } else { self.next() % n }
        }
    }

    /// Checks the ring's counters against the model.
    fn check_counts(ring: &RingBuffer<u8>, model: &alloc::collections::VecDeque<u8>, cap: usize) {
        assert_eq!(ring.capacity(), cap);
        assert_eq!(ring.len(), model.len());
        assert_eq!(ring.window(), cap - model.len());
        assert_eq!(ring.is_empty(), model.is_empty());
        assert_eq!(ring.is_full(), model.len() == cap);
        assert!(ring.contiguous_window() <= ring.window());
    }

    /// Runs `steps` random operations on `ring` and on a `VecDeque` model of what it holds, and checks every result:
    /// what each copy moves, every slice's bytes, and that a slice is never longer than `max_piece` (a page) and never
    /// empty while there is something to give.
    fn run_model(mut ring: RingBuffer<u8>, cap: usize, max_piece: usize, seed: u64, steps: usize) {
        let mut model = alloc::collections::VecDeque::new();
        let mut rng = Rng(seed);
        let mut next_byte = 0u8;
        let mut fresh = |n: usize| -> alloc::vec::Vec<u8> {
            (0..n)
                .map(|_| {
                    next_byte = next_byte.wrapping_add(1);
                    next_byte
                })
                .collect()
        };
        for _ in 0..steps {
            let room = cap - model.len();
            match rng.below(9) {
                0 => {
                    let data = fresh(rng.below(2 * cap + 2));
                    let n = ring.enqueue_slice(&data);
                    assert_eq!(n, cmp::min(data.len(), room));
                    model.extend(&data[..n]);
                }
                1 => {
                    let mut out = alloc::vec![0; rng.below(2 * cap + 2)];
                    let n = ring.dequeue_slice(&mut out);
                    assert_eq!(n, cmp::min(out.len(), model.len()));
                    for b in &out[..n] {
                        assert_eq!(Some(*b), model.pop_front());
                    }
                }
                2 => {
                    // TCP's out-of-order receive: write a later part first, then the start, then enqueue both.
                    let total = rng.below(room + 1);
                    let split = rng.below(total + 1);
                    let data = fresh(total);
                    assert_eq!(ring.write_unallocated(split, &data[split..]), total - split);
                    assert_eq!(ring.write_unallocated(0, &data[..split]), split);
                    ring.enqueue_unallocated(total);
                    model.extend(&data);
                }
                3 => {
                    let offset = rng.below(model.len() + 2);
                    let mut out = alloc::vec![0; rng.below(cap + 2)];
                    let n = ring.read_allocated(offset, &mut out);
                    let avail = model.len().saturating_sub(offset);
                    assert_eq!(n, cmp::min(out.len(), avail));
                    for (i, b) in out[..n].iter().enumerate() {
                        assert_eq!(*b, model[offset + i]);
                    }
                }
                4 => {
                    let offset = rng.below(model.len() + 1);
                    let size = rng.below(cap + 2);
                    let slice = ring.get_allocated(offset, size);
                    let avail = cmp::min(size, model.len() - offset);
                    assert!(slice.len() <= avail && slice.len() <= max_piece);
                    assert_eq!(slice.is_empty(), avail == 0);
                    for (i, b) in slice.iter().enumerate() {
                        assert_eq!(*b, model[offset + i]);
                    }
                }
                5 => {
                    let want = rng.below(cap + 2);
                    let data = fresh(want);
                    let slice = ring.enqueue_many(want);
                    let n = slice.len();
                    assert!(n <= cmp::min(want, room) && n <= max_piece);
                    assert_eq!(n == 0, want == 0 || room == 0);
                    slice.copy_from_slice(&data[..n]);
                    model.extend(&data[..n]);
                }
                6 => {
                    let want = rng.below(cap + 2);
                    let held = model.len();
                    let slice = ring.dequeue_many(want);
                    assert!(slice.len() <= cmp::min(want, held) && slice.len() <= max_piece);
                    assert_eq!(slice.is_empty(), want == 0 || held == 0);
                    for b in slice.iter() {
                        assert_eq!(Some(*b), model.pop_front());
                    }
                }
                7 => {
                    let n = rng.below(model.len() + 1);
                    ring.dequeue_allocated(n);
                    model.drain(..n);
                }
                _ => {
                    if rng.below(2) == 0 {
                        let b = fresh(1)[0];
                        match ring.enqueue_one() {
                            Ok(slot) => {
                                *slot = b;
                                model.push_back(b);
                            }
                            Err(Full) => assert_eq!(room, 0),
                        }
                    } else {
                        match ring.dequeue_one() {
                            Ok(b) => assert_eq!(Some(*b), model.pop_front()),
                            Err(Empty) => assert!(model.is_empty()),
                        }
                    }
                }
            }
            check_counts(&ring, &model, cap);
        }
    }

    #[test]
    fn test_paged_matches_model() {
        for &(cap, page) in &[
            (1, 1),
            (7, 1),
            (7, 3),
            (8, 4),
            (9, 4),
            (64, 7),
            (64, 64),
            (64, 100),
            (100, 16),
            (257, 32),
        ] {
            for seed in 1..=20u64 {
                let ring = RingBuffer::new_paged(cap, page).unwrap();
                run_model(ring, cap, cmp::min(page, cap), seed * 7919 + cap as u64, 3000);
            }
        }
    }

    #[test]
    fn test_slice_matches_model() {
        // The same model on the one-slice storage: the copy loops must leave its behaviour as it was.
        for &cap in &[1usize, 2, 7, 64, 100] {
            for seed in 1..=20u64 {
                run_model(RingBuffer::new(alloc::vec![0u8; cap]), cap, cap, seed * 104729 + cap as u64, 3000);
            }
        }
    }

    #[test]
    fn test_paged_pages_and_zero_capacity() {
        let ring: RingBuffer<u8> = RingBuffer::new_paged(10, 4).unwrap();
        match &ring.storage {
            Storage::Paged { pages, page, len } => {
                assert_eq!((*page, *len), (4, 10));
                assert_eq!(pages.iter().map(|p| p.len()).collect::<alloc::vec::Vec<_>>(), [4, 4, 2]);
            }
            _ => panic!("not paged"),
        }
        let mut empty: RingBuffer<u8> = RingBuffer::new_paged(0, 4096).unwrap();
        assert_eq!(empty.capacity(), 0);
        assert_eq!(empty.get_unallocated(0, 0), &[]);
        assert_eq!(empty.get_allocated(0, 0), &[]);
        assert_eq!(empty.enqueue_many(1), &[]);
        assert_eq!(empty.enqueue_slice(b"x"), 0);
        assert_eq!(empty.enqueue_one(), Err(Full));
        assert_eq!(empty.contiguous_window(), 0);
    }

    #[test]
    fn test_paged_slice_stops_at_page_end() {
        let mut ring = RingBuffer::new_paged(12, 4).unwrap();
        assert_eq!(ring.enqueue_slice(b"abcdefghij"), 10);
        // From index 0, a slice holds at most one page.
        assert_eq!(ring.get_allocated(0, 10), b"abcd");
        assert_eq!(ring.get_allocated(2, 10), b"cd");
        assert_eq!(ring.get_allocated(4, 10), b"efgh");
        assert_eq!(ring.get_allocated(8, 10), b"ij");
        assert_eq!(ring.dequeue_many(3), b"abc");
        assert_eq!(ring.dequeue_many(3), b"d");
        let mut out = [0u8; 6];
        assert_eq!(ring.read_allocated(0, &mut out), 6);
        assert_eq!(&out, b"efghij");
        // Writes wrap round the storage and across pages.
        assert_eq!(ring.enqueue_slice(b"klmnopqr"), 6);
        let mut all = [0u8; 12];
        assert_eq!(ring.dequeue_slice(&mut all), 12);
        assert_eq!(&all, b"efghijklmnop");
    }
}
