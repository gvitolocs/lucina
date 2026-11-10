use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    any::Any,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use ax_sync::SpinLock;
use usb_if::{
    descriptor::EndpointType,
    endpoint::{
        EndpointInfo, IsoPacketResult, RequestId, TransferBuffer, TransferCompletion, TransferKind,
        TransferRequest, TransferStatus,
    },
    err::TransferError,
    host::ControlSetup,
    transfer::Direction,
};

#[cfg(any(kmod, umod))]
use super::transfer::Transfer;

mod ctrl;

/// A transfer and the buffer it moves into the endpoint (unsafe audit F-01). `usb_if::TransferRequest` holds a raw
/// pointer with no lifetime, so a borrowed buffer could be freed while the request is in flight, and the completion
/// would then copy the received bytes into freed memory. This one owns its buffer: it stays with the endpoint until
/// [`EndpointHandle::reclaim`] hands it back in an [`OwnedCompletion`]. If the request fails, is abandoned, or the
/// endpoint goes away first, the buffer is freed with the request's DMA mapping, or kept with it in the controller's
/// quarantine while the controller may still use it.
pub struct OwnedRequest {
    kind: TransferKind,
    direction: Direction,
    buffer: Vec<u8>,
}

impl OwnedRequest {
    fn new(kind: TransferKind, direction: Direction, buffer: Box<[u8]>) -> Self {
        Self {
            kind,
            direction,
            buffer: buffer.into_vec(),
        }
    }

    pub fn control_in(setup: ControlSetup, buffer: Box<[u8]>) -> Self {
        Self::new(TransferKind::Control(setup), Direction::In, buffer)
    }

    pub fn control_out(setup: ControlSetup, buffer: Box<[u8]>) -> Self {
        Self::new(TransferKind::Control(setup), Direction::Out, buffer)
    }

    pub fn bulk_in(buffer: Box<[u8]>) -> Self {
        Self::new(TransferKind::Bulk, Direction::In, buffer)
    }

    pub fn bulk_out(buffer: Box<[u8]>) -> Self {
        Self::new(TransferKind::Bulk, Direction::Out, buffer)
    }

    pub fn interrupt_in(buffer: Box<[u8]>) -> Self {
        Self::new(TransferKind::Interrupt, Direction::In, buffer)
    }

    pub fn interrupt_out(buffer: Box<[u8]>) -> Self {
        Self::new(TransferKind::Interrupt, Direction::Out, buffer)
    }

    pub fn iso_in(buffer: Box<[u8]>, packet_lengths: &[usize]) -> Self {
        let packet_lengths = packet_lengths.to_vec();
        Self::new(TransferKind::Isochronous { packet_lengths }, Direction::In, buffer)
    }

    pub fn iso_out(buffer: Box<[u8]>, packet_lengths: &[usize]) -> Self {
        let packet_lengths = packet_lengths.to_vec();
        Self::new(TransferKind::Isochronous { packet_lengths }, Direction::Out, buffer)
    }

    pub fn direction(&self) -> Direction {
        self.direction
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

/// A finished transfer and the buffer it was submitted with (the received bytes for IN are
/// `buffer[..actual_length]`).
#[derive(Debug)]
pub struct OwnedCompletion {
    pub request_id: RequestId,
    pub status: TransferStatus,
    pub actual_length: usize,
    pub iso_packets: Vec<IsoPacketResult>,
    pub buffer: Box<[u8]>,
}

impl OwnedCompletion {
    pub(crate) fn new(completion: TransferCompletion, buffer: Vec<u8>) -> Self {
        Self {
            request_id: completion.request_id,
            status: completion.status,
            actual_length: completion.actual_length,
            iso_packets: completion.iso_packets,
            buffer: buffer.into_boxed_slice(),
        }
    }
}

/// A request the endpoint did not take. `buffer` is the request's buffer, untouched, when the endpoint refused it
/// before using it: always for a revoked or disconnected endpoint or the wrong endpoint type, and on xHCI and EHCI
/// for `QueueFull` and a dead controller too. The DWC2 and libusb backends do not give it back.
#[derive(Debug)]
pub struct SubmitError {
    pub error: TransferError,
    pub buffer: Option<Box<[u8]>>,
}

impl From<SubmitError> for TransferError {
    fn from(error: SubmitError) -> Self {
        error.error
    }
}

/// What a backend refuses a submission with: the error, and the buffer if it was not used.
pub(crate) type Rejected = (TransferError, Option<Vec<u8>>);

/// What a backend gets to submit: the `usb_if` request a backend understands, whose buffer pointer is into
/// `buffer`'s heap allocation and nothing else. Only [`OwnedRequest`] makes one, so the pair always matches. The
/// backend keeps `buffer` with the request's DMA state until it is done with the request; moving the `Vec` does not
/// move its heap buffer, so the pointer stays valid.
pub(crate) struct Submission {
    request: TransferRequest,
    buffer: Vec<u8>,
}

impl From<OwnedRequest> for Submission {
    fn from(owned: OwnedRequest) -> Self {
        let OwnedRequest {
            kind,
            direction,
            mut buffer,
        } = owned;
        let ptr = TransferBuffer::from_mut_slice(&mut buffer);
        Self {
            request: (kind, direction, ptr).into(),
            buffer,
        }
    }
}

impl Submission {
    pub(crate) fn request(&self) -> &TransferRequest {
        &self.request
    }

    pub(crate) fn into_parts(self) -> (TransferRequest, Vec<u8>) {
        (self.request, self.buffer)
    }

    /// Refuses this submission with `error`, handing its unused buffer back.
    pub(crate) fn reject(self, error: TransferError) -> Rejected {
        (error, Some(self.buffer))
    }
}

pub(crate) trait EndpointOp: Send + Any + 'static {
    /// Takes the submission's buffer and keeps it with the request until `reclaim_request` gives it back, or until
    /// nothing may access it any more (the request failed, or was abandoned or dropped, and its DMA state freed or
    /// quarantined with the buffer).
    fn submit_request(&mut self, submission: Submission) -> Result<RequestId, Rejected>;

    fn reclaim_request(&mut self, id: RequestId)
    -> Option<Result<OwnedCompletion, TransferError>>;

    fn register_waker(&self, id: RequestId, cx: &mut Context<'_>);

    fn cancel_request(&mut self, _id: RequestId) -> Result<(), TransferError> {
        Err(TransferError::NotSupported)
    }

    fn abandon_request(&mut self, _id: RequestId) -> Result<(), TransferError> {
        Err(TransferError::NotSupported)
    }

    fn reset(&mut self) -> EndpointResetFuture {
        Box::pin(async { Err(TransferError::NotSupported) })
    }
}

pub type EndpointResetFuture =
    Pin<Box<dyn Future<Output = Result<(), TransferError>> + Send + 'static>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EndpointLifecycle {
    Active,
    Revoked,
    Disconnected,
}

struct EndpointInner {
    raw: Box<dyn EndpointOp>,
    lifecycle: EndpointLifecycle,
}

#[derive(Clone)]
pub struct EndpointHandle {
    info: EndpointInfo,
    inner: Arc<SpinLock<EndpointInner>>,
}

impl EndpointHandle {
    #[cfg(any(kmod, umod))]
    pub(crate) fn new(info: EndpointInfo, raw: impl EndpointOp) -> Self {
        Self {
            info,
            inner: Arc::new(SpinLock::new(EndpointInner {
                raw: Box::new(raw),
                lifecycle: EndpointLifecycle::Active,
            })),
        }
    }

    pub fn info(&self) -> EndpointInfo {
        self.info
    }

    /// Submits `request`; its buffer belongs to the endpoint until [`reclaim`](Self::reclaim) or
    /// [`poll_request`](Self::poll_request) returns it. A refused request may give it back (see [`SubmitError`]).
    pub fn submit(&self, request: OwnedRequest) -> Result<RequestId, SubmitError> {
        let submission = Submission::from(request);
        let refused = |(error, buffer): Rejected| SubmitError {
            error,
            buffer: buffer.map(Vec::into_boxed_slice),
        };
        if let Err(error) = self.validate_request(submission.request()) {
            return Err(refused(submission.reject(error)));
        }
        let mut inner = self.inner.lock();
        match inner.lifecycle {
            EndpointLifecycle::Active => inner.raw.submit_request(submission).map_err(refused),
            EndpointLifecycle::Revoked => {
                Err(refused(submission.reject(TransferError::EndpointRevoked)))
            }
            EndpointLifecycle::Disconnected => {
                Err(refused(submission.reject(TransferError::Disconnected)))
            }
        }
    }

    pub fn reclaim(&self, id: RequestId) -> Result<Option<OwnedCompletion>, TransferError> {
        match self.inner.lock().raw.reclaim_request(id) {
            Some(result) => result.map(Some),
            None => Ok(None),
        }
    }

    pub fn poll_request(
        &self,
        id: RequestId,
        cx: &mut Context<'_>,
    ) -> Poll<Result<OwnedCompletion, TransferError>> {
        let mut inner = self.inner.lock();
        match inner.raw.reclaim_request(id) {
            Some(res) => Poll::Ready(res),
            None => {
                inner.raw.register_waker(id, cx);
                match inner.raw.reclaim_request(id) {
                    Some(res) => Poll::Ready(res),
                    None => Poll::Pending,
                }
            }
        }
    }

    pub fn cancel(&self, id: RequestId) -> Result<(), TransferError> {
        self.inner.lock().raw.cancel_request(id)
    }

    /// Forgets a transfer whose completion was lost, and releases its DMA mapping. Only for a transfer the controller
    /// is known to be past, because a later transfer on this endpoint has finished: a controller works through an
    /// endpoint's ring in order.
    pub fn abandon(&self, id: RequestId) -> Result<(), TransferError> {
        self.inner.lock().raw.abandon_request(id)
    }

    /// Resets host-controller state for this endpoint after a successful
    /// `CLEAR_FEATURE(ENDPOINT_HALT)` request.
    pub fn reset(&self) -> EndpointResetFuture {
        self.inner.lock().raw.reset()
    }

    /// Submits `request` and waits for it. Dropping the future before it completes leaves the request in flight, its
    /// buffer still owned by the endpoint.
    pub async fn wait(&self, request: OwnedRequest) -> Result<OwnedCompletion, TransferError> {
        let id = self.submit(request)?;
        EndpointRequestFuture { id, endpoint: self }.await
    }

    #[allow(unused)]
    pub(crate) fn with_raw_mut<T: EndpointOp, R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut inner = self.inner.lock();
        let d = inner.raw.as_mut() as &mut dyn Any;
        f(d.downcast_mut::<T>()
            .expect("EndpointHandle downcast_mut failed"))
    }

    pub(crate) fn revoke(&self) {
        self.inner.lock().lifecycle = EndpointLifecycle::Revoked;
    }

    pub(crate) fn disconnect(&self) {
        self.inner.lock().lifecycle = EndpointLifecycle::Disconnected;
    }

    pub(crate) fn reactivate(&self) {
        self.inner.lock().lifecycle = EndpointLifecycle::Active;
    }

    fn validate_request(&self, request: &TransferRequest) -> Result<(), TransferError> {
        let request_type = match request {
            TransferRequest::Control { .. } => EndpointType::Control,
            TransferRequest::Bulk { .. } => EndpointType::Bulk,
            TransferRequest::Interrupt { .. } => EndpointType::Interrupt,
            TransferRequest::Isochronous { .. } => EndpointType::Isochronous,
        };
        if request_type == self.info.transfer_type {
            Ok(())
        } else {
            Err(TransferError::InvalidEndpoint)
        }
    }
}

struct EndpointRequestFuture<'a> {
    id: RequestId,
    endpoint: &'a EndpointHandle,
}

impl Future for EndpointRequestFuture<'_> {
    type Output = Result<OwnedCompletion, TransferError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.endpoint.poll_request(this.id, cx)
    }
}

/// The completion of a finished `transfer`, with the buffer it was submitted with (`buffer`).
#[cfg(any(kmod, umod))]
pub(crate) fn transfer_to_owned_completion(
    id: RequestId,
    transfer: &Transfer,
    buffer: Vec<u8>,
) -> OwnedCompletion {
    let iso_packets = match &transfer.kind {
        usb_if::endpoint::TransferKind::Isochronous { packet_lengths } => packet_lengths
            .iter()
            .copied()
            .zip(transfer.iso_packet_actual_lengths.iter().copied())
            .map(|(requested_length, actual_length)| IsoPacketResult {
                requested_length,
                actual_length,
                status: TransferStatus::Completed,
            })
            .collect(),
        _ => Vec::new(),
    };

    OwnedCompletion::new(
        TransferCompletion {
            request_id: id,
            status: TransferStatus::Completed,
            actual_length: transfer.transfer_len,
            iso_packets,
        },
        buffer,
    )
}

/// The completion of a finished `transfer`: its mapping is dropped and its buffer handed back.
#[cfg(kmod)]
pub(crate) fn transfer_to_completion(id: RequestId, mut transfer: Transfer) -> OwnedCompletion {
    let buffer = transfer.take_buffer();
    transfer_to_owned_completion(id, &transfer, buffer)
}
