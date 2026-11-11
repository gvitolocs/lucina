# LucinaOS changes to crab-usb 0.12.1

This is crab-usb 0.12.1 as published on crates.io (checksum in `references/upstream.lock`). Its first commit in this
repository is unchanged; `git log -p -- third_party/crab-usb` shows every change since. `kernel/inject.txt` copies it
into the kernel tree and `aster-usb` depends on that copy by path (patch 0181). Offering a change upstream is a draft
only, without the owner's OK.

**Not `[patch.crates-io]`:** the kernel is built from OSDK's base crate, which is its own workspace and copies only
`[profile]` from the kernel's root `Cargo.toml`, so a `[patch]` there is ignored without a word. The first version of
this fix went that way and never reached the kernel: the laptop failed the same. `LUCINA_REVISION` exists so that this
cannot happen again: `aster-usb` uses it (it logs `crab-usb 0.12.1, LucinaOS revision N`), and the kernel does not
compile against any other copy.

Licence: `Cargo.toml` says Apache-2.0, the included `LICENSE` file is MIT. Either allows this.

## Endpoint lookup in the active configuration (`src/backend/kmod/xhci/device.rs`)

`find_interface_endpoints` searched all configurations and took the first interface number that matched. On a device
whose configurations reuse interface numbers with different endpoints, claiming an interface configured the endpoints
of the wrong configuration.

Seen on a Realtek RTL8152 (0bda:8152) on the Galaxy Book Go. Configuration 1 is vendor-specific, with interface 0 holding
bulk 0x81 and 0x02. Configuration 2 is CDC-ECM, with those endpoints on interface 1. With configuration 2 selected,
`claim_interface(0, 0)` took configuration 1's endpoints, and `claim_interface(1, 1)` then failed with
`InvalidParameter` because interface 0 already owned them. QEMU's usb-net does not show this: its endpoints sit on the
same interfaces in both configurations.

The lookup now only searches the active configuration. Before any configuration is selected it searches all, as before.

## Every `InvalidParameter` on the claim path says why

`claim interface N: endpoint 0x81 belongs to interface 0`, `claim interface N: already claimed`, and the two session
checks. `InvalidParameter` itself has no room for a reason.

## Event and TRB access order (`src/backend/kmod/xhci/event.rs`, `ring.rs`)

On the Galaxy Book Go, whose controller does not snoop the CPU, a sustained transfer ended with `transfer event points
outside active ring`, and the network never came back. `EventRing::next` read the event with a plain 16-byte load and
then looked at its cycle bit, so the new cycle bit could go with the pointer of the ring's previous lap. It now reads
the cycle word, then `rmb()`, then the rest, as Linux's `xhci_handle_event`. TRBs had the same problem from the other
side: they are now written with the cycle word last, after `wmb()`, as Linux's `queue_trb`. The accesses are volatile.

An event TRB of an unknown type no longer stops the event ring.

An event off its ring logs its full pointer in lines that fit the screen, and which ring the pointer is on: another
endpoint's points at the read order above, none at the controller.

`EndpointHandle::abandon` lets a driver forget a transfer whose completion was lost; `aster-usb-net`'s pipes use it.
`LUCINA_TEAR_TRANSFER_EVENTS` gives events the previous event's pointer on purpose, to test all of that end to end.

## Linux's xHCI and hub behaviour where ours differed (driver audit D1-D11, 2026-11-04)

The audit `checklists/usb.md` of the Juno driver audit compared this stack with Linux 7.1 (`drivers/usb/host/xhci*`,
`drivers/usb/core/hub.c`). Each difference without a recorded reason now follows Linux; the values and the order are
Linux's, the code is not. Polling stays (no interrupt line), and the event read order above is unchanged.

**Bounded waits (D1).** `osal.rs` has `wait_until` and `handshake` (Linux `xhci_handshake`: a register reading all ones
ends the wait as "gone"). The sleeps are the crate's `KernelOp::delay`; between two polls the future yields once, so the
kernel's `block_on` drains the event rings and a command that completes at once costs no sleep. Bring-up (`host.rs`) is
`xhci_gen_setup`'s order: halt (R/S and the interrupt enables cleared, HCH within 32 ms, `XHCI_MAX_HALT_USEC`), then
`xhci_reset` (skipped with a warning if not halted, HCRST then CNR clear within 10 s each, `XHCI_RESET_LONG_USEC`); the
extra CNR wait before HCRST is gone. Start waits 32 ms for HCH to clear. The BIOS handoff runs only if BIOS Owned is set,
waits 1 s and then takes the controller anyway, as `quirk_usb_handoff_xhci`. A command (`cmd.rs`) fails after 5 s
(`XHCI_CMD_DEFAULT_TIMEOUT`), as `xhci_handle_command_timeout`: a Stop Endpoint that times out kills the controller;
otherwise, if CRR is set, CRCR is written with CA and the pointer of the next TRB, CRR must clear within 5 s (else the
controller is halted and dead), then up to 2 s for the Command Ring Stopped event; the command's TRB becomes a No Op
with its cycle bit kept (`trb_to_noop`). Every event-loop pass checks USBSTS as `xhci_irq` does, once the controller
runs: all ones, HCE or HSE halts the controller and marks it dead; commands and transfers then fail with `NoDevice`
(Linux gives the URBs back with -ESHUTDOWN).

**Command ring slots.** A `TWaiter` gives its slot back when dropped (`queue.rs`). Before, `take_waiter` panicked on the
512th command, the first that reuses a slot of the 2-page ring; the endpoint recovery below sends more commands.

**PORTSC (D2).** Every PORTSC write is `port_state_to_neutral(read)` (Linux `xhci_port_state_to_neutral`: RO and RWS
kept, PED and the RW1C change bits never written back) plus the bits meant, through `PortStatusRegisters::write_neutral_*`
(`reg.rs`); the closure-based read-modify-write is gone. The root hub (`xhci/hub.rs`) powers the ports, waits the
power-on-to-good time of Linux's xHCI root hubs (20 ms for USB 2 ports, 100 ms for USB 3, `xhci-hub.c` bPwrOn2PwrGood),
and resets nothing at start. Which ports are USB 3 comes from the Supported Protocol capabilities (`reg.rs`
`apply_supported_protocol`). A connected port, or one with a connect change, is debounced as `hub_port_debounce`
(25 ms steps, stable 100 ms, 2 s, every CSC seen cleared, first sample never counts) and then reported; the
enumeration attempt resets it. That reset is `hub_port_reset` / `hub_port_wait_reset` (`hub/port.rs` holds the shared
rules): a hot reset (PR), or a warm reset (WPR) for a USB 3 link in SS.Inactive, Compliance Mode or with CAS set; first
wait 60 ms (`HUB_ROOT_RESET_TIME`, 200 ms for low speed), then 200 ms steps up to 800 ms, up to 5 resets, the change
bits cleared, 50 ms recovery. Linux hot-resets a connected USB 3 port too at enumeration, so this does. A USB 3 link
that needs it gets a warm reset when the port has no device (`port_event`). Unlike Linux, a port whose connection is
gone is not debounced: Linux spends up to 2 s on it in its hub thread, here it would stall the one USB thread.

**Enumeration retries (D5).** `kcore.rs` runs Linux's `hub_port_connect` loop: up to 4 attempts (`PORT_INIT_TRIES`),
each starting with the port reset above (`HubOp::reset_port_for_enumeration`), a power cycle of the port after the
second, a stop when the device is gone, and at the end the port is disabled (`HubOp::give_up_port`: PED on a USB 2 root
port, U3 on a USB 3 one, ClearPortFeature(ENABLE) or link U3 on an external hub) and left until its connection
changes. A failed attempt releases its device (`Device::release_failed`: Stop Endpoint, Disable Slot, endpoint routes
dropped, so the next Enable Slot can reuse the slot ID). One port's failure no longer drops the other events of the
hub, nor one hub's the other hubs'. External hubs (`hub/device.rs`) use the same debounce and reset (10 ms first wait,
`HUB_SHORT_RESET_TIME`); their power-good wait is the descriptor's, at least 100 ms. Hubs that reset ports themselves
before reporting them (DWC2, EHCI) get one attempt, as before. Inside one attempt the xHCI device (`xhci/device.rs`
`init`) follows `hub_port_init` in its Set Address first order (Linux's old scheme, the order of this crate): up to 2
rounds 100 ms apart (`GET_DESCRIPTOR_TRIES`), each with the Address Device command up to 2 times, 200 ms after a
failure (`SET_ADDRESS_TRIES`), then 10 ms, then the 8-byte descriptor read up to 3 times (`GET_MAXPACKET0_TRIES`,
`get_bMaxPacketSize0`: a device descriptor with bMaxPacketSize0 8, 16, 32, 64 or 9; a timeout in the first round of a
high or super speed device ends the round). A halted EP0 is reset before each read. Not ported: the new scheme
(Address Device with BSR=1 and a 64-byte read first).

**EP0 max packet from the descriptor.** As `hub_port_init`: at SuperSpeed and above bMaxPacketSize0 is a log2 (9 is
512); before, 9 was written to EP0's context as 9 bytes, and 0 became 8. Full and high speed accept 8 to 64, low
speed 8 only, anything else fails the attempt; EP0 is evaluated again only when the size differs from the default.

**Endpoint halts (D7).** As `xhci_halted_host_endpoint`, Babble, USB Transaction Error and Split Transaction Error now
halt a non-isochronous endpoint like Stall did, so the driver's `reset()` (Reset Endpoint, Set TR Dequeue) recovers it.
A USB Transaction Error on a bulk or interrupt endpoint of a device not behind a TT is first soft-retried, as
`process_bulk_intr_td`: while the error count (reset by a success) was at most `MAX_SOFT_RETRY` (3) the TD is kept, a
Reset Endpoint with TSP is sent and the doorbell rung (`xhci_handle_cmd_reset_ep`). Linux's exception for a babbling
control endpoint on xHCI 0.95 hardware is not ported.

**SuperSpeed hubs (D4).** Port status of a SuperSpeed hub is decoded with the USB 3 layout (power bit 9, link state
bits 8:5, speed SuperSpeed), other hubs with the USB 2 layout.

**TT think time (D6).** 666 ns x (TT bits + 1), 0 only for 8 FS bit times on a full speed hub (hub.c:1609-1635); the slot
field stays `ns / 666 - 1` (xhci.c:5338-5343), so 8/16/24/32 FS bit times give 0/1/2/3 (was 0/0/1/2).

**64-bit registers (D8).** DCBAAP, CRCR, ERSTBA and ERDP are written as two 32-bit writes at their xHCI offsets
(`XhciRegisters::write_*`). Linux writes them low word first (`xhci_write_64`); only ERSTBA follows
`XHCI_WRITE_64_HI_LO` (xhci-mem.c:2346), which every DWC3 sets (`write-64-hi-lo-quirk`, dwc3/host.c:168). The audit
and the spec said all four go high word first; Linux's source says ERSTBA only, and that is what is done. The order is a
parameter: `USBHost::new_xhci_with_write64_order(.., Write64Order::HiLo)` for a DWC3 core; `new_xhci` keeps Linux's
plain order. The kernel's DWC3 glue still calls `new_xhci` and must switch to get the quirk. The Rockchip `Dwc` glue
passes `HiLo`. CRCR and ERSTBA keep their low six bits as read, as Linux.

**EP0 max packet (D9).** SuperSpeedPlus default 512, as xhci-mem.c.

**Values of ours (D10).** Marked in comments: the 200 ms after start, IMOD 0x1F, HSEE and EWE, the ring sizes (command
ring 2 pages, event ring 2 segments, endpoint rings 16 pages), the poll steps of the bounded waits. The 50/100 ms hub
port reset waits and the 500 ms enable wait are gone (Linux's reset replaces them). The polling periods, the USB thread
cadence and the halt_all poll count are in the kernel's `aster-usb`, not here.

**DWC3 identity (D3, D11).** Nothing is written. The Rockchip `Dwc` glue logs GSNPSID, GHWPARAMS0-8 (GHWPARAMS8,
0xc600, is now mapped) and GUSB3PIPECTL(0) at start. The laptop does not use that glue: its DWC3 globals are mapped by
the kernel's own `dwc3.rs`, so on the laptop this log needs the kernel side.

**Test races.** `the_64_bit_registers_are_at_their_xhci_offsets` built its register file in a `Vec<u32>`, 4-byte
aligned, and the xhci crate asserts that the interrupter registers are 32-byte aligned: it failed about one run in ten,
with the allocator's placement. The file is now page aligned. The xHCI endpoint tests counted streaming mappings in
one static that all tests share in parallel, and `stopped_iso_request_publishes_cancelled_exactly_once` asserts it;
the count is now per test thread.

**DWC2 rearm.** `rearm_irq` unmasks only what `acknowledge_irq` masked: it used to re-enable the whole runtime mask and
undo the HCHINT mask a disconnect sets (Linux `dwc2_hcd_disconnect`; `new_device` unmasks it again). Found because
`disconnect_event_stops_and_publishes_disconnected_completion` failed once the test lock runtime below let it run.

## DMA memory the controller may still use is quarantined, never freed (unsafe audit F-02, F-03, 2026-11-05; revision 4)

On Juno (SC7180) the controller sits on the SMMU in bypass: its addresses are physical, so memory freed while it may still
write there is reused for page tables or heap and gets corrupted, and the laptop resets with no crash report. The rule now:
while it is not proven that the controller stopped touching a buffer, the buffer goes into the controller's quarantine
(`src/backend/kmod/xhci/quarantine.rs`) and stays there for good. Linux frees in these cases; its IOMMU turns a late DMA
into a fault and ours cannot, so this is an *adapt*. A leak is the lesser evil.

- **One quarantine per controller (F-03).** The rings of a slot whose Disable Slot failed went into a `Vec` inside the
  `Device`, and `new_device` dropped the device right after `release_failed()`, freeing them. The quarantine is now on
  the command ring that the host, every device and every endpoint share, so it outlives any device. Every former
  `quarantined_eps` push uses it. Nothing drains it: this crate has no reset of a dead controller (HCH, then HCRST)
  that would prove the controller stopped. A device dropped while its slot is not known to be disabled also keeps its
  device context (which the controller writes) and its rings there.
- **Dead is not halted (F-02).** `halt_now` returns whether HCH was seen (registers reading all ones count as not seen).
  The command ring records it (`is_halted`; `die` and the event handler's fault check set it). A dead controller not
  seen to halt may still run its schedules: `reclaim_request` still returns `NoDevice`, and `abandon_request` still
  forgets the TD, but the TD (its bounce buffer and streaming mapping) goes into the quarantine instead of being
  dropped. A controller seen to halt frees as before.
- Tests: `a_dead_controller_not_seen_to_halt_keeps_the_td_buffer`, `..._keeps_an_abandoned_td_buffer` and
  `a_dead_controller_seen_to_halt_frees_the_td_buffer` (endpoint.rs, the streaming mapping count);
  `a_failed_disable_slot_keeps_the_rings_and_context_after_the_device_is_dropped` and
  `a_failed_disable_slot_on_a_halted_controller_frees_the_device` (device.rs, a count of live coherent allocations in
  the test DMA kernel); `a_fault_without_hch_leaves_the_controller_dead_but_not_halted` (host.rs).

## A transfer owns its buffer (unsafe audit F-01, 2026-11-05; revision 5)

`EndpointHandle::submit` took a `usb_if::TransferRequest`, whose buffer is a raw pointer with no lifetime, and returned a
`RequestId` that borrowed nothing. Safe code could free the buffer while the transfer was in flight; completing an IN then
copied the bounce buffer into freed memory (dma-api `sync_map_for_cpu`), and an OUT read freed memory at submit. That was
unsound for every safe caller, including the kernel's `deny(unsafe_code)` crates. `usb-if` is the registry crate 0.8.0, so
the fix is in this crate's API only.

- `submit` takes an `OwnedRequest` (`bulk_in(Box<[u8]>)`, `bulk_out`, `interrupt_*`, `control_*`, `iso_*`) and its buffer
  moves into the endpoint; `reclaim`, `poll_request` and `wait` return an `OwnedCompletion` with the buffer back
  (`buffer`, plus the fields of `TransferCompletion`). There is no public way to submit a borrowed buffer any more: the
  F-01 reproducer (`ep.submit(TransferRequest::bulk_in(&mut stack_buf))`) no longer compiles.
- The backends keep the buffer with the TD's DMA state (`Transfer::owned` for xHCI and EHCI, declared after the mapping so
  the mapping is unmapped first; `Dwc2DmaBuffer::owned`; the libusb handle for `umod`). Whatever frees a TD frees its
  buffer, and whatever quarantines a TD (a controller dead and not seen to halt, an abandoned TD on it, a device whose
  slot is not known to be disabled) keeps its buffer too. A failed or abandoned transfer does not give its buffer back.
- A refused submission returns `SubmitError { error, buffer }`, with the buffer when it was not used (a revoked or
  disconnected endpoint, the wrong type; on xHCI and EHCI also `QueueFull` and a dead controller), so a caller can retry
  with the same data.
  `TransferError: From<SubmitError>`.
- `wait` stays safe: dropping its future leaves the request in flight with the endpoint owning the buffer. The control
  helpers (`control_in`/`control_out` and the descriptor reads on top of them) keep their borrowed signatures and copy
  through an owned buffer.
- An xHCI `Endpoint` dropped with TDs in flight on a controller dead and not seen to halt quarantines them (as
  `release_td`); otherwise the device only lets an endpoint go once its slot is disabled, as before.
- Tests (endpoint.rs): the test DMA kernel now bounces, as the kernel's glue does, and a test can write a mapping's bounce
  buffer as the device. `an_owned_bulk_in_buffer_comes_back_with_the_received_bytes` (a completion copies into the owned
  buffer and hands it back after unmapping), `an_endpoint_dropped_with_a_request_in_flight_keeps_its_buffer` and
  `an_endpoint_dropped_on_a_live_controller_frees_its_tds`. Miri cannot run these: every streaming sync goes through
  `mbarrier::mb()`, inline assembly that Miri does not support.

The kernel's callers (`aster-usb` storage and HID, `aster-usb-net` pipes and AX88179) move to the owned API in patch 1062.

## A cancelled transfer is taken off the ring as Linux kills an URB (Juno patch 1196b, 2026-11-11; revision 6)

`cancel_request` on xHCI only set a flag, and the request stayed in flight until its own event. It now follows
`xhci_urb_dequeue`: the TD joins `cancelled_tds` (Linux `cancelled_td_list`) and a pass starts (`cancel_op`) unless one
runs or a soft retry holds the endpoint; a TD cancelled meanwhile joins the next pass. A TD whose completion is in
already is not cancelled (`usb_start_wait_urb` returns the completion of a transfer that finished as the timeout fired).

- Stop Endpoint (`xhci_handle_cmd_stop_ep`): a context state error is read against the endpoint state in the output
  device context. Halted: Reset Endpoint (no TSP) first, and the TD the endpoint halted on is stepped over too
  (`find_halted_td`). Stopped: go on. Running: stop again (at most 3 times, ours; Linux has no bound), then the pass
  is given up and the TDs' buffers go to the quarantine.
- Invalidate (`xhci_invalidate_cancelled_tds`): the TR Dequeue Pointer and DCS come from the endpoint context of the
  output device context (`xhci_get_hw_deq`; new `OutContext`, shared with the device, `Endpoint::set_out_context`);
  without it, from a Stopped event. The cancelled TD holding that pointer gets a Set TR Dequeue Pointer to the TRB after
  its last (`Ring::past_td`, `xhci_move_dequeue_past_td`: walked from the hardware pointer, the cycle state toggled at
  the Link TRB); every other cancelled TD is turned into Transfer No-Ops that keep only their cycle bit
  (`Ring::td_to_noop`, `trb_to_noop`). Linux 6.12 clears the chain bit and the other fields; the task text said to keep
  the chain bit, this follows the source.
- Give back (`xhci_handle_cmd_set_deq`, `xhci_giveback_invalidated_tds`): each cancelled TD is dropped (buffer and
  mapping freed, or quarantined if the Set TR Dequeue Pointer failed for the TD the controller held, or the controller
  is dead and not halted), `reclaim_request` returns `Err(Cancelled)`, and the doorbell restarts the remaining TDs.
  The Stopped events of the TDs that go on are cleared first: a slot holds one event and drops a second.
- `reclaim_request` drives the pass (as it does a soft retry) and returns `None` for a cancelled TD until it ends.
  A Stopped, Stopped-Length-Invalid or Stopped-Short-Packet event on a TD that is not cancelled is not a completion
  (`process_*_td`); it used to end the request with an error.
- `submit_request` drives a pass its owner left unpolled (a cancel followed by a drop) and, while one still runs, queues
  the TD without the doorbell, as Linux's `xhci_ring_ep_doorbell` does for `EP_STOP_CMD_PENDING | SET_DEQ_PENDING`;
  the end of the pass rings it. The new TD is no part of the pass (`first_new_id`): it lies behind the enqueue point of
  the stop, is never turned into No-Ops and the new dequeue pointer does not reach it. No-Op TRBs still ahead of the
  controller count against the ring's room (`stale_noops`) until a later TD completes.
- `Endpoint::stop_future`, which `Device::stop_endpoints` / `_disconnect` send before a reconfiguration, no longer sends
  Stop Endpoint to an endpoint whose output-context state is already Stopped (a cancel pass leaves it so when no TD is
  left to restart): the Context State Error failed `set_alternate`, and every later stream start with it. Linux sends no
  Stop to reconfigure and takes the error on a stopped endpoint as done (`xhci_handle_cmd_stop_ep`). A halted endpoint
  is reset first, a running one that answers with the error is stopped again (at most 3 times). The cancel pass reads
  the error against the same state (`stop_state`).
- `EndpointHandle::wait_with_id` returns a `RequestWait` that keeps the request id, so a caller with a deadline can
  `cancel` it and go on polling the same wait until it ends.

Tests: `ring.rs` (the new dequeue pointer and DCS, the wrap over the Link TRB, the No-Op conversion), `context.rs` (the
output context read), `endpoint.rs` (the whole pass against a played controller: No-Op path, Set TR Dequeue Pointer
path, a failed Set TR Dequeue Pointer, halted endpoint, endless race, joined pass, completion before cancel, Stopped
events).

## Tests

The tests are in the xHCI `device.rs` (`lucina_tests`, run by `scripts/ci.sh`), with the descriptors from the laptop's
screen. `published_lookup_reproduces_the_laptop_failure` replays the CDC driver's claims with 0.12.1's lookup and gets
the laptop's failure; `active_configuration_lookup_claims_both_interfaces` is the same with the fix. The tests stop at
this check: the rest of a claim needs a controller, and QEMU's usb-net cannot reach this failure. This directory also has a `Cargo.lock`, for that
test only: the kernel build uses the kernel tree's lock.

`cargo test` now runs the whole crate: `src/test_lock_runtime.rs` gives the host tests the lock runtime that ax-sync's
locks call (a kernel provides it). The stub it replaces panicked in every test that takes a lock, which was 24 of 83.
The new tests cover the neutral PORTSC mask, the Supported Protocol map, the register offsets and the ERSTBA order, the
debounce counter, the reset schedule and retries, the TT mapping, the enumeration retry count and power cycle
(`kcore.rs`), the command timeout and the stop-event wait (fake clock), the descriptor header check, the EP0 max packet decode,
the address and descriptor retry counts, the soft retry count, and a Host System Error,
or registers reading all ones, marking the controller dead.
