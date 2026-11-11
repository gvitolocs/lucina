use alloc::{sync::Arc, vec::Vec};

use dma_api::{CoherentArray, CoherentBox, ContiguousArray, DmaDirection};
use xhci::context::{Device32Byte, Device64Byte, Input32Byte, Input64Byte, InputHandler};

use super::SlotId;
use crate::{err::*, osal::Kernel};

pub struct DeviceContextList {
    dcbaa: CoherentArray<u64>,
    max_slots: usize,
}

pub(crate) struct Context32 {
    out: Arc<CoherentBox<Device32Byte>>,
    input: CoherentBox<Input32Byte>,
}

pub(crate) struct Context64 {
    out: Arc<CoherentBox<Device64Byte>>,
    input: CoherentBox<Input64Byte>,
}
pub(crate) enum ContextData {
    Context32(Context32),
    Context64(Context64),
}

/// The controller's view of a slot, its output device context, for reading what the controller wrote into an
/// endpoint context: the endpoint state and the TR Dequeue Pointer it stopped on (Linux `xhci_get_hw_deq`,
/// `GET_EP_CTX_STATE`). A shared handle, so an endpoint can read it for as long as the endpoint lives.
#[derive(Clone)]
pub(crate) enum OutContext {
    Context32(Arc<CoherentBox<Device32Byte>>),
    Context64(Arc<CoherentBox<Device64Byte>>),
}

/// Endpoint context states (xHCI 6.2.3.2, Linux `EP_STATE_*`).
pub(crate) const EP_STATE_RUNNING: u8 = 1;
pub(crate) const EP_STATE_HALTED: u8 = 2;
pub(crate) const EP_STATE_STOPPED: u8 = 3;

impl OutContext {
    /// The first four dwords of the context of endpoint `dci` (1..=31); dword 0 holds the state, dwords 2 and 3 the
    /// TR Dequeue Pointer with the Dequeue Cycle State in bit 0.
    fn endpoint_words(&self, dci: u8) -> [u32; 4] {
        assert!((1..=31).contains(&dci));
        let (base, context_size) = match self {
            OutContext::Context32(out) => (out.as_ptr().as_ptr().cast::<u8>(), 32),
            OutContext::Context64(out) => (out.as_ptr().as_ptr().cast::<u8>(), 64),
        };
        // SAFETY: the slot context is followed by 31 endpoint contexts, so the context of `dci` and its first
        // four dwords are inside the allocation `self` keeps alive. Volatile: the controller writes it.
        let endpoint = unsafe { base.add(usize::from(dci) * context_size) }.cast::<u32>();
        core::array::from_fn(|word| unsafe { endpoint.add(word).read_volatile() })
    }

    /// The endpoint state the controller reports.
    pub fn endpoint_state(&self, dci: u8) -> u8 {
        (self.endpoint_words(dci)[0] & 0x7) as u8
    }

    /// The TR Dequeue Pointer and Dequeue Cycle State of a stopped endpoint.
    pub fn endpoint_dequeue(&self, dci: u8) -> (u64, bool) {
        let words = self.endpoint_words(dci);
        let pointer = (u64::from(words[3]) << 32) | u64::from(words[2]);
        (pointer & !0xf, words[2] & 1 != 0)
    }
}

impl ContextData {
    /// Writes dword `word` of the endpoint context `dci` of the output context as the controller would.
    #[cfg(test)]
    pub(crate) fn controller_writes(&self, dci: u8, word: usize, value: u32) {
        let (base, size) = match self {
            ContextData::Context32(ctx) => (ctx.out.as_ptr().as_ptr().cast::<u8>(), 32),
            ContextData::Context64(ctx) => (ctx.out.as_ptr().as_ptr().cast::<u8>(), 64),
        };
        // SAFETY: inside the output context allocation, as in `OutContext::endpoint_words`.
        unsafe {
            base.add(usize::from(dci) * size)
                .cast::<u32>()
                .add(word)
                .write_volatile(value)
        };
    }

    pub fn out_context(&self) -> OutContext {
        match self {
            ContextData::Context32(ctx) => OutContext::Context32(ctx.out.clone()),
            ContextData::Context64(ctx) => OutContext::Context64(ctx.out.clone()),
        }
    }

    pub fn new(is_64: bool, dma: &Kernel) -> core::result::Result<Self, HostError> {
        if is_64 {
            Ok(ContextData::Context64(Context64 {
                out: Arc::new(dma.coherent_box_zero_with_align(64)?),
                input: dma.coherent_box_zero_with_align(64)?,
            }))
        } else {
            Ok(ContextData::Context32(Context32 {
                out: Arc::new(dma.coherent_box_zero_with_align(64)?),
                input: dma.coherent_box_zero_with_align(64)?,
            }))
        }
    }

    pub fn with_empty_input<F>(&mut self, f: F)
    where
        F: FnOnce(&mut dyn InputHandler),
    {
        match self {
            ContextData::Context32(ctx) => {
                let mut input = Input32Byte::new_32byte();
                f(&mut input);
                ctx.input.write_cpu(input);
            }
            ContextData::Context64(ctx) => {
                let mut input = Input64Byte::new_64byte();
                f(&mut input);
                ctx.input.write_cpu(input);
            }
        }
    }

    pub fn with_input<F>(&mut self, f: F)
    where
        F: FnOnce(&mut dyn InputHandler),
    {
        match self {
            ContextData::Context32(ctx) => {
                let mut input = ctx.input.read_cpu();
                f(&mut input);
                ctx.input.write_cpu(input);
            }
            ContextData::Context64(ctx) => {
                let mut input = ctx.input.read_cpu();
                f(&mut input);
                ctx.input.write_cpu(input);
            }
        }
    }

    pub fn dcbaa(&self) -> u64 {
        match self {
            ContextData::Context32(ctx) => ctx.out.dma_addr(),
            ContextData::Context64(ctx) => ctx.out.dma_addr(),
        }
        .as_u64()
    }

    pub fn input_bus_addr(&self) -> u64 {
        match self {
            ContextData::Context32(ctx) => ctx.input.dma_addr(),
            ContextData::Context64(ctx) => ctx.input.dma_addr(),
        }
        .as_u64()
    }

    pub fn perper_change(&mut self) {
        self.with_input(|input| {
            let control_context = input.control_mut();
            for i in 0..32 {
                control_context.clear_add_context_flag(i);
                if i > 1 {
                    control_context.clear_drop_context_flag(i);
                }
            }
            control_context.set_add_context_flag(0);
        });
    }
}

impl DeviceContextList {
    pub fn new(max_slots: usize, dma: &Kernel) -> Result<Self> {
        let dcbaa = dma
            .coherent_array_zero_with_align(256, dma.page_size())
            .map_err(|_| USBError::NoMemory)?;
        Ok(Self { dcbaa, max_slots })
    }

    pub fn new_ctx(&mut self, slot_id: SlotId, is_64: bool, dma: &Kernel) -> Result<ContextData> {
        if slot_id.as_usize() > self.max_slots {
            Err(USBError::SlotLimitReached)?;
        }
        let ctx = ContextData::new(is_64, dma)?;
        self.dcbaa.set_cpu(slot_id.as_usize(), ctx.dcbaa());
        Ok(ctx)
    }

    pub(crate) fn bus_addr(&self) -> u64 {
        self.dcbaa.dma_addr().as_u64()
    }

    pub(crate) fn set_scratchpad_array(&mut self, bus_addr: u64) {
        self.dcbaa.set_cpu(0, bus_addr);
    }
}

pub struct ScratchpadBufferArray {
    entries: CoherentArray<u64>,
    _pages: Vec<ContiguousArray<u8>>,
}

impl ScratchpadBufferArray {
    pub fn new(entries: usize, dma: &Kernel) -> Result<Self> {
        let mut entries_vec = dma
            .coherent_array_zero_with_align(entries, 64)
            .map_err(|_| USBError::NoMemory)?;

        let mut pages: Vec<ContiguousArray<u8>> = Vec::with_capacity(entries_vec.len());
        for _ in 0..entries_vec.len() {
            let page = dma
                .contiguous_array_zero_with_align(
                    dma.page_size(),
                    dma.page_size(),
                    DmaDirection::Bidirectional,
                )
                .map_err(|_| USBError::NoMemory)?;
            page.prepare_for_device(0..page.bytes_len());
            pages.push(page);
        }

        // 将每个页面的地址写入到 entries 数组中
        for (i, page) in pages.iter().enumerate() {
            entries_vec.set_cpu(i, page.dma_addr().as_u64());
        }

        Ok(Self {
            entries: entries_vec,
            _pages: pages,
        })
    }

    pub fn bus_addr(&self) -> u64 {
        self.entries.dma_addr().as_u64()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::kmod::xhci::endpoint::tests::test_kernel;

    #[test]
    fn the_endpoint_state_and_dequeue_pointer_are_read_from_the_output_context() {
        for is_64 in [false, true] {
            let ctx = ContextData::new(is_64, &test_kernel()).unwrap();
            let out = ctx.out_context();
            ctx.controller_writes(3, 0, 0xffff_fff8 | u32::from(EP_STATE_STOPPED));
            ctx.controller_writes(3, 2, 0x1234_5671);
            ctx.controller_writes(3, 3, 0x0000_0007);
            assert_eq!(out.endpoint_state(3), EP_STATE_STOPPED);
            assert_eq!(out.endpoint_dequeue(3), (0x7_1234_5670, true));
            // Another endpoint's context is not touched by those writes.
            assert_eq!(out.endpoint_state(2), 0);
            assert_eq!(out.endpoint_dequeue(2), (0, false));
        }
    }
}
