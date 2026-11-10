use alloc::vec::Vec;
use core::ptr::NonNull;

use dma_api::DmaDirection;
use usb_if::{err::TransferError, transfer::Direction};

use crate::{
    backend::ty::{
        ep::Submission,
        transfer::{Transfer, TransferKind},
    },
    osal::Kernel,
};

const ALIGN: usize = 64;

impl Transfer {
    pub(crate) fn new(
        dma: &Kernel,
        kind: TransferKind,
        direction: Direction,
        buff: Option<(NonNull<u8>, usize)>,
        owned: Vec<u8>,
    ) -> Result<Self, TransferError> {
        let dma_direction = match direction {
            Direction::In => DmaDirection::FromDevice,
            Direction::Out => DmaDirection::ToDevice,
        };
        let mapping = if let Some((ptr, len)) = buff.filter(|(_, len)| *len > 0) {
            // SAFETY: `ptr..ptr + len` is `owned`'s heap buffer (`Submission` pairs a request only with the buffer
            // it was built from), which this transfer keeps, and nothing else accesses it until the transfer gives it back in
            // `take_buffer` after dropping the mapping.
            let slice = unsafe { core::slice::from_raw_parts_mut(ptr.as_ptr(), len) };
            Some(
                dma.map_streaming_slice(slice, ALIGN, dma_direction)
                    .map_err(|err| TransferError::Other(anyhow!("DMA mapping failed: {err}")))?,
            )
        } else {
            None
        };

        Ok(Self {
            kind,
            direction,
            mapping,
            owned,
            transfer_len: 0,
            iso_packet_actual_lengths: Vec::new(),
        })
    }

    pub(crate) fn from_submission(
        dma: &Kernel,
        submission: Submission,
    ) -> Result<Self, TransferError> {
        let (request, owned) = submission.into_parts();
        let (kind, direction, buffer) = request.into();
        let buff = buffer.map(|buffer| (buffer.ptr, buffer.len));
        Self::new(dma, kind, direction, buff, owned)
    }

    /// Unmaps the buffer and hands it back: the transfer is finished, no CPU sync or DMA touches it any more.
    pub(crate) fn take_buffer(&mut self) -> Vec<u8> {
        self.mapping = None;
        core::mem::take(&mut self.owned)
    }

    pub fn buffer_len(&self) -> usize {
        if let Some(ref mapping) = self.mapping {
            mapping.len()
        } else {
            0
        }
    }

    pub fn dma_addr(&self) -> u64 {
        if let Some(ref mapping) = self.mapping {
            mapping.dma_addr().as_u64()
        } else {
            0
        }
    }

    pub fn complete_for_cpu(&self) {
        if let Some(ref mapping) = self.mapping {
            mapping.complete_for_cpu(0..mapping.bytes_len());
        }
    }

    pub fn prepare_for_device(&self) {
        if let Some(ref mapping) = self.mapping {
            mapping.prepare_for_device(0..mapping.bytes_len());
        }
    }
}
