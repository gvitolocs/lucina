//! The shell's end of one app's surface socket: the messages of `docs/design/juno-window-manager.md`
//! section 5a and the descriptor handover that rides with them.
//!
//! Section 5a gives every running app one `SOCK_SEQPACKET` socket, made by the shell before exec and inherited as a low
//! descriptor; the app never opens a path. This module is the wire and the socket and nothing else: no compositor, no policy,
//! no `apps.rs`. Slice W hands the app's end to the spawn path and answers what arrives here.
//!
//! # The frame
//!
//! Every message is one datagram, little endian:
//!
//! | Offset | Type | Field |
//! | --- | --- | --- |
//! | 0 | `u8` | [`WIRE_VERSION`] |
//! | 1 | `u8` | the tag: which message the payload is |
//! | 2 | `u16` | payload length |
//! | 4 | | the payload |
//!
//! A frame is at most [`MAX_MSG`] bytes, so what a peer can make the shell hold is bounded before a field of it is read.
//! Strings carry a `u16` count and are refused past their own bound ([`MAX_TITLE`], [`MAX_ID`]); a [`Damage::Parts`] row past
//! [`MAX_DAMAGE`] is refused; a surface side past [`MAX_DIM`] is refused. Every count a peer controls is checked against the
//! bytes that are actually there before those bytes are read.
//!
//! [`Wire::encode`] and [`Wire::decode`] are the whole codec: pure, no I/O, no panic. [`Wire::decode`] answers anything it does
//! not like with a [`Decode`] — a truncated frame, an unknown tag, a length that disagrees with the payload, invalid UTF-8, a
//! field out of range — and its only callers are [`Link::recv`] and [`Link::recv_from_shell`]. [`Wire::encode`] refuses in the
//! same way, by returning None, so a sender that hands over a title longer than the wire's own bound is told about it instead
//! of emitting a frame no peer reads.
//!
//! # The descriptors
//!
//! `surface` and `frame` carry buffer descriptors as `SCM_RIGHTS` ancillary data. They are *not* counted in the frame: the
//! frame says how many buffers a surface has ([`ToShell::Surface::buffers`], one or two) and the kernel says how many came, and
//! [`Link::recv`] refuses a `surface` whose two numbers disagree rather than believing the one in the payload. So a peer has
//! nothing to contradict, and a decoder has no count to leak by.
//!
//! At most [`MAX_FDS`] come with one message: `recvmsg` is given the cmsg space for exactly that many, so a peer that
//! sends more is answered by `MSG_CTRUNC`, which is an error here rather than a short read. Every descriptor the kernel
//! handed over on such a path is owned by an [`Fds`] and so closed on the way out: an error costs no descriptor. A message
//! that may not carry descriptors at all (`hello`, `title`, `state-saved`, `bye`) and arrives with them has told the same
//! kind of lie as a wrong payload length, and is refused the same way. Received descriptors are marked close-on-exec, because
//! the shell is the one process that forks: the kernel does not do it for us (a test below says so), and without it one
//! app's buffer would be handed to the address space of the next app that starts.
//!
//! # What section 5a leaves open, and what is chosen here
//!
//! - `title` is bounded at [`MAX_TITLE`] bytes, the 64 of the doc's row. `hello`'s app id has no bound in the doc, so it takes
//!   the same 64 ([`MAX_ID`]); no manifest id in the tree comes near it. What an id may contain stays the codec's no-business:
//!   the shell checks a decoded id against the manifest it launched, which is what the doc's row asks of it.
//! - `configure`'s scale is one `u8`, a whole-number factor (1 is 1:1, 2 is a HiDPI panel). A fraction would need a
//!   denominator to say nothing Juno has today: every screen in the tree is 1x.
//! - `frame`'s buffers are named by index, and the doc gives that message no descriptor of its own. It may still carry up to
//!   [`MAX_FDS`], because a GPU fence has nowhere else to travel with the frame it fences; nothing requires it to, so a frame
//!   with none is fine and a frame with four is not.
//! - `surface`'s format is one byte with the one value section 5a names ([`FORMAT_ARGB8888`]); another value is refused, so a
//!   second format is a protocol change and not a surprise.
//! - Presentation time and input timestamps are `i64` microseconds of `CLOCK_MONOTONIC`, the shape of
//!   [`lucina_app::input::Event::at_us`], so `input` carries the kernel's record untouched.
//! - The tags of the two directions share one numbering rather than each starting at 1: a frame is one direction's, but a log
//!   line that says `tag 3` should mean one thing.
//!
//! The outbox: [`Link::send`] never waits on an app. It writes straight through while the socket takes the message, and from
//! there on it waits in a queue of at most [`OUTBOX_MAX`]. When that is full, a sample (`input`, `frame-done`) gives its
//! place to a newer one and the oldest is dropped; a state message (`configure`, `background`, `foreground`) replaces the older
//! message of its kind, since only the last one says where the app is, and otherwise takes a sample's place; `close` is refused
//! with `EOVERFLOW`, because an app the shell has told to end must hear it or the caller must hear why. Nothing else is refused:
//! a dropped stale event is worth more than a wedged socket.

#![allow(dead_code)] // slice W wires this in: no shell code holds a `Link` until the spawn path hands one over

use std::collections::VecDeque;
use std::fmt;
use std::io::{self, ErrorKind};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};

use lucina_app::input::Event;
/// The protocol version both ends speak: in `hello` and as the first byte of every frame. A peer naming another number is a
/// different build, and the shell answers it with the crash screen rather than by guessing at its fields.
pub const PROTO_VERSION: u32 = 1;
/// The first byte of every frame: [`PROTO_VERSION`] as the framing carries it.
const WIRE_VERSION: u8 = PROTO_VERSION as u8;
/// The lowest descriptor the app's end may take. 3 is the hold pipe, dup'd onto it by `apps.rs` and named by
/// [`lucina_app::hold::FD`], so the surface socket asks for the next free slot at or above this and the number it gets is what
/// the child is told.
pub const APP_FD: i32 = 4;
/// The environment variable that names [`APP_FD`] to the child, so an app finds its socket without opening a path. The shape
/// of `LUCINA_HOLD_FD`.
pub const APP_FD_ENV: &str = "LUCINA_SURFACE_FD";
/// Descriptors at most with one message: a double-buffered `surface` and one fence, which is all a frame can need.
pub const MAX_FDS: usize = 3;
/// Buffers a `surface` may name. Section 5a hands the app one or two; the third slot of [`MAX_FDS`] is the fence, not a
/// third buffer, so a surface that names three is a mistake and not a mode.
pub const MAX_BUFFERS: u8 = 2;
/// Bytes in a frame, header and payload both: what a peer can make the shell hold.
pub const MAX_MSG: usize = 256;
/// The longest `title`, in bytes of UTF-8 (section 5a).
pub const MAX_TITLE: usize = 64;
/// The longest `hello` app id, in bytes. See the module's list of choices.
pub const MAX_ID: usize = 64;
/// Damage rectangles in one `frame` before the app must say [`Damage::Full`] instead (section 5a: "up to 4, else full").
pub const MAX_DAMAGE: usize = 4;
/// `surface`'s only format today.
pub const FORMAT_ARGB8888: u8 = 0;
/// The longest side of a surface or a configure, in pixels: 4096, the value `lucina-surface-rules` names `MAX_DIM` and which
/// the shell does not depend on, so it is written down twice. A buffer the rules would refuse has no business on this wire
/// either; raising one of the two without the other is the mistake to watch.
pub const MAX_DIM: u16 = 0x4000;
/// Messages [`Link::send`] will hold for an app that is not reading.
pub const OUTBOX_MAX: usize = 64;

/// The tag byte, per message.
mod tag {
    pub const HELLO: u8 = 1;
    pub const SURFACE: u8 = 2;
    pub const FRAME: u8 = 3;
    pub const TITLE: u8 = 4;
    pub const STATE_SAVED: u8 = 5;
    pub const BYE: u8 = 6;
    pub const CONFIGURE: u8 = 7;
    pub const FRAME_DONE: u8 = 8;
    pub const INPUT: u8 = 9;
    pub const BACKGROUND: u8 = 10;
    pub const FOREGROUND: u8 = 11;
    pub const CLOSE: u8 = 12;
    /// Bytes of a frame before its payload.
    pub const HEADER: usize = 4;
    /// [`Damage::Full`] in a `frame` payload.
    pub const DAMAGE_FULL: u8 = 0;
    /// [`Damage::Parts`] in a `frame` payload, then a count and that many rectangles.
    pub const DAMAGE_PARTS: u8 = 1;
}

/// A damage rectangle: the part of a buffer that changed. The shell clips it; the codec only bounds how many arrive, so a
/// rectangle outside the surface costs a copy of nothing rather than a panic somewhere else.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

/// What one frame says changed: the whole surface, or these rectangles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Damage {
    /// Everything: the app scrolled, resized, or cannot say.
    Full,
    /// The first `n` of the row, with `n` in 1 ..= [`MAX_DAMAGE`]. The rest is zeroed, by [`Damage::parts`] and by the codec,
    /// so two `Damage`s naming the same rectangles compare equal.
    Parts([Rect; MAX_DAMAGE], u8),
}

impl Damage {
    /// The rectangles, padding included: read [`Damage::len`] of them, and none for [`Damage::Full`].
    pub fn rects(&self) -> &[Rect] {
        match self {
            Damage::Full => &[],
            Damage::Parts(r, _) => r,
        }
    }

    /// How many of [`Damage::rects`] mean something: 0 for [`Damage::Full`].
    pub fn len(&self) -> usize {
        match self {
            Damage::Full => 0,
            Damage::Parts(_, n) => *n as usize,
        }
    }

    /// Nothing changed at all.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// These rectangles as a [`Damage`], or None for none of them or more than [`MAX_DAMAGE`] — past four, section 5a says, the
    /// app sends [`Damage::Full`].
    pub fn parts(rects: &[Rect]) -> Option<Damage> {
        if rects.is_empty() || rects.len() > MAX_DAMAGE {
            return None;
        }
        let mut row = [Rect::default(); MAX_DAMAGE];
        row[..rects.len()].copy_from_slice(rects);
        Some(Damage::Parts(row, rects.len() as u8))
    }
}

/// The descriptors that came with one message, or that go with one. It owns them, so dropping an `Fds` closes what is in it:
/// that is how an error path costs no descriptor.
#[derive(Debug)]
pub struct Fds {
    slots: [Option<OwnedFd>; MAX_FDS],
}

impl Fds {
    /// None.
    pub fn new() -> Fds {
        Fds {
            slots: std::array::from_fn(|_| None),
        }
    }

    /// Take `fd` into the next free slot, or give it back when all [`MAX_FDS`] are taken.
    pub fn push(&mut self, fd: OwnedFd) -> Result<(), OwnedFd> {
        for slot in self.slots.iter_mut() {
            if slot.is_none() {
                *slot = Some(fd);
                return Ok(());
            }
        }
        Err(fd)
    }

    /// How many descriptors it holds.
    pub fn len(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The `i`th, for the shell to hand to the compositor or to close.
    pub fn get(&self, i: usize) -> Option<&OwnedFd> {
        self.slots.get(i)?.as_ref()
    }

    /// Take the `i`th out, leaving nothing behind: the compositor takes a buffer this way and the `Fds` keeps the rest.
    pub fn take(&mut self, i: usize) -> Option<OwnedFd> {
        self.slots.get_mut(i)?.take()
    }

    /// The numbers of them, for one `sendmsg`.
    fn raw(&self) -> Vec<RawFd> {
        self.slots
            .iter()
            .filter_map(|s| s.as_ref().map(AsRawFd::as_raw_fd))
            .collect()
    }
}

impl Default for Fds {
    fn default() -> Fds {
        Fds::new()
    }
}

/// Two rows of descriptors are equal when they hold the same number of them. A descriptor has no equality worth naming: two
/// numbers for one open file are not the same name, and one name for two different files is not the same buffer. A count is all
/// a codec round trip can mean, and both sides of one are empty.
impl PartialEq for Fds {
    fn eq(&self, other: &Fds) -> bool {
        self.len() == other.len()
    }
}

impl Eq for Fds {}

/// What arrives from an app. The `fds` of a `surface` or a `frame` are the descriptors that came beside it, so
/// [`Wire::encode`] leaves them out of the frame and [`Wire::decode`] leaves them empty: only [`Link::recv`] fills them.
#[derive(Debug, PartialEq)]
pub enum ToShell {
    /// An app's first message: which build it is, and which app it claims to be. The shell checks the id against the manifest
    /// it launched.
    Hello { version: u32, app_id: String },
    /// A surface the app will paint into, with the descriptors that came beside the frame in `fds`.
    Surface {
        width: u16,
        height: u16,
        format: u8,
        /// One or two ([`MAX_BUFFERS`]): the row the app paints into, single or double buffered.
        buffers: u8,
        fds: Fds,
    },
    /// A buffer is ready to show.
    Frame {
        index: u8,
        damage: Damage,
        /// The app wants [`ToApp::FrameDone`] for this frame, which is how it paces itself.
        wants_frame: bool,
        fds: Fds,
    },
    /// For the switcher and the overview: at most [`MAX_TITLE`] bytes of UTF-8.
    Title(String),
    /// The answer to [`ToApp::Background`]: the state is on disk, the app may go.
    StateSaved,
    /// A clean exit; the shell then reaps the process.
    Bye,
}

/// What arrives from the shell. `Copy`: these are a few words, and the outbox moves them about by value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToApp {
    /// The size to paint (the screen, or a tile), the scale, and whether it has the focus. Where the tile *is* stays the
    /// shell's: section 5a, the app never knows its position.
    Configure {
        width: u16,
        height: u16,
        scale: u8,
        focused: bool,
    },
    /// Buffer `index` is free to paint into again, at this `CLOCK_MONOTONIC` time.
    FrameDone { index: u8, at_us: i64 },
    /// One kernel input record, for the focused app only.
    Input(Event),
    /// Off screen: save, drop caches. Answered by [`ToShell::StateSaved`].
    Background,
    /// Back on screen.
    Foreground,
    /// What F1 does now: SIGTERM semantics, then SIGKILL after the grace.
    Close,
}

impl ToApp {
    /// A message whose value decays: a newer one is worth more than the oldest queued, and an old one is worth almost nothing.
    fn is_sample(&self) -> bool {
        matches!(self, ToApp::Input(_) | ToApp::FrameDone { .. })
    }

    /// Whether `self` makes `old` pointless, so the queue may drop `old` for it. Only the last size and the last visibility
    /// state mean anything to an app. Never two samples: a key press and its release are both `input`, and losing one would
    /// leave a key down.
    fn supersedes(&self, old: &ToApp) -> bool {
        use ToApp::{Background, Configure, Foreground};
        matches!(
            (self, old),
            (Configure { .. }, Configure { .. })
                | (Background | Foreground, Background | Foreground)
        )
    }
}

/// A frame that is not one. Every arm is something a peer controls, and every field is checked against the bytes that are
/// there, so no arm means an index out of bounds in the reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decode {
    /// Fewer bytes than the header, or fewer than the payload the header promises.
    TooShort,
    /// More bytes than the payload the header promises.
    Trailing,
    /// The frame does not start with [`WIRE_VERSION`].
    Version(u8),
    /// A tag that means nothing in this direction.
    Tag(u8),
    /// A payload longer than a frame may be.
    Len(u16),
    /// A string past its bound.
    TooLong {
        what: &'static str,
        max: usize,
        got: usize,
    },
    /// A string that is not UTF-8.
    Utf8 { what: &'static str },
    /// A field whose value this protocol does not mean: a side past [`MAX_DIM`] or zero, a format other than
    /// [`FORMAT_ARGB8888`], a buffer count outside 1 ..= [`MAX_BUFFERS`], a damage row of four rects or of none counted, a
    /// flag that is neither 0 nor 1.
    Range { what: &'static str, got: u32 },
}

impl fmt::Display for Decode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Decode::TooShort => f.write_str("the frame ends before its payload does"),
            Decode::Trailing => f.write_str("bytes are left over after the payload"),
            Decode::Version(v) => write!(f, "protocol version {v}, not {WIRE_VERSION}"),
            Decode::Tag(t) => write!(f, "no message for tag {t}"),
            Decode::Len(n) => write!(f, "a payload of {n} bytes, past what a frame may carry"),
            Decode::TooLong { what, max, got } => write!(f, "{what} of {got} bytes, past {max}"),
            Decode::Utf8 { what } => write!(f, "{what} is not UTF-8"),
            Decode::Range { what, got } => {
                write!(f, "{what} is {got}, which this protocol does not mean")
            }
        }
    }
}

impl std::error::Error for Decode {}

impl From<Decode> for io::Error {
    fn from(d: Decode) -> io::Error {
        io::Error::new(ErrorKind::InvalidData, d)
    }
}

/// The payload reader: a position and a length, nothing else. Every read is a `get` with the bounds in it, so a peer that lies
/// about a length stops here with a [`Decode`] rather than in an index.
///
/// Public because [`Wire::body_from`] takes one: a message's payload is read through this cursor, so anything that implements
/// or calls that method names the type. [`decode`](Wire::decode) is the way to read a whole frame; this is for a caller that
/// already split the header off.
pub struct Cur<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Cur<'a> {
    /// A cursor over `b`, at its start.
    pub fn new(b: &'a [u8]) -> Cur<'a> {
        Cur { b, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Decode> {
        let end = self.pos.checked_add(n).ok_or(Decode::TooShort)?;
        let s = self.b.get(self.pos..end).ok_or(Decode::TooShort)?;
        self.pos = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, Decode> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, Decode> {
        let s = self.take(2)?;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }

    fn u32(&mut self) -> Result<u32, Decode> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }

    fn i32(&mut self) -> Result<i32, Decode> {
        let s = self.take(4)?;
        Ok(i32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }

    fn i64(&mut self) -> Result<i64, Decode> {
        let s = self.take(8)?;
        Ok(i64::from_le_bytes([
            s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
        ]))
    }

    fn boolean(&mut self) -> Result<bool, Decode> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(Decode::Range {
                what: "flag",
                got: other as u32,
            }),
        }
    }

    /// A `u16` count and that many bytes, as a String.
    fn text(&mut self, what: &'static str, max: usize) -> Result<String, Decode> {
        let n = self.u16()? as usize;
        if n > max {
            return Err(Decode::TooLong { what, max, got: n });
        }
        let s = self.take(n)?;
        std::str::from_utf8(s)
            .map(str::to_owned)
            .map_err(|_| Decode::Utf8 { what })
    }

    /// The payload with nothing left unread.
    fn done(&mut self) -> Result<(), Decode> {
        if self.pos != self.b.len() {
            return Err(Decode::Trailing);
        }
        Ok(())
    }
}

fn put(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(bytes);
}

fn boolean(out: &mut Vec<u8>, v: bool) {
    out.push(u8::from(v));
}

/// A string as a `u16` count and its bytes, or None when it is longer than `max`: the sender broke the bound, and nothing is
/// written.
fn text(out: &mut Vec<u8>, s: &str, max: usize) -> Option<()> {
    if s.len() > max {
        return None;
    }
    put(out, &(s.len() as u16).to_le_bytes());
    put(out, s.as_bytes());
    Some(())
}

/// `s` cut to at most `max` bytes, on a character boundary: for a title or an id that arrives longer than the wire allows,
/// which is what a toolkit hands over more often than not.
pub fn clamp_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// A side of a surface: in 1 ..= [`MAX_DIM`]. Zero is not a size anyone can paint into.
fn dim(v: u16, what: &'static str) -> Result<u16, Decode> {
    if v == 0 || v > MAX_DIM {
        return Err(Decode::Range {
            what,
            got: v as u32,
        });
    }
    Ok(v)
}

/// The framing both directions share: a tag, a body, and the four bytes around them.
pub trait Wire: Sized {
    /// The tag byte of this message.
    fn tag(&self) -> u8;

    /// The payload, appended. None when the message breaks one of the wire's bounds, with `out` as it was.
    fn body(&self, out: &mut Vec<u8>) -> Option<()>;

    /// The message a payload says, `p` naming exactly the bytes the header counted.
    fn body_from(tag: u8, p: &mut Cur<'_>) -> Result<Self, Decode>;

    /// The whole frame, appended to `out`, or None when the message is bigger than a frame may be. Its return is the length of
    /// that frame.
    fn encode(&self, out: &mut Vec<u8>) -> Option<usize> {
        let at = out.len();
        out.push(WIRE_VERSION);
        out.push(self.tag());
        put(out, &[0, 0]);
        if self.body(out).is_none() {
            out.truncate(at);
            return None;
        }
        let n = out.len() - at - tag::HEADER;
        if n > MAX_MSG - tag::HEADER {
            out.truncate(at);
            return None;
        }
        out[at + 2..at + 4].copy_from_slice(&(n as u16).to_le_bytes());
        Some(n + tag::HEADER)
    }

    /// The frame's bytes, or None for a message that breaks a bound.
    fn to_vec(&self) -> Option<Vec<u8>> {
        let mut v = Vec::with_capacity(MAX_MSG);
        self.encode(&mut v)?;
        Some(v)
    }

    /// One frame read.
    fn decode(bytes: &[u8]) -> Result<Self, Decode> {
        let head = bytes.get(..tag::HEADER).ok_or(Decode::TooShort)?;
        if head[0] != WIRE_VERSION {
            return Err(Decode::Version(head[0]));
        }
        let len = u16::from_le_bytes([head[2], head[3]]) as usize;
        if len > MAX_MSG - tag::HEADER {
            return Err(Decode::Len(len as u16));
        }
        let want = tag::HEADER + len;
        if bytes.len() < want {
            return Err(Decode::TooShort);
        }
        if bytes.len() > want {
            return Err(Decode::Trailing);
        }
        let mut p = Cur::new(&bytes[tag::HEADER..]);
        let m = Self::body_from(head[1], &mut p)?;
        p.done()?;
        Ok(m)
    }
}

impl Wire for ToShell {
    fn tag(&self) -> u8 {
        use ToShell::{Bye, Frame, Hello, StateSaved, Surface, Title};
        match self {
            Hello { .. } => tag::HELLO,
            Surface { .. } => tag::SURFACE,
            Frame { .. } => tag::FRAME,
            Title(_) => tag::TITLE,
            StateSaved => tag::STATE_SAVED,
            Bye => tag::BYE,
        }
    }

    fn body(&self, out: &mut Vec<u8>) -> Option<()> {
        use ToShell::{Bye, Frame, Hello, StateSaved, Surface, Title};
        match self {
            Hello { version, app_id } => {
                put(out, &version.to_le_bytes());
                text(out, app_id, MAX_ID)
            }
            Surface {
                width,
                height,
                format,
                buffers,
                fds: _,
            } => {
                put(out, &width.to_le_bytes());
                put(out, &height.to_le_bytes());
                out.push(*format);
                out.push(*buffers);
                Some(())
            }
            Frame {
                index,
                damage,
                wants_frame,
                fds: _,
            } => {
                out.push(*index);
                boolean(out, *wants_frame);
                match damage {
                    Damage::Full => out.push(tag::DAMAGE_FULL),
                    Damage::Parts(row, n) => {
                        out.push(tag::DAMAGE_PARTS);
                        out.push(*n);
                        // The padding of the row stays out of the frame: `n` says how many rects there are, and the reader
                        // below takes exactly that many, so writing the whole array here would leave bytes it never consumes
                        // and every round trip of a damaged frame would end in `Trailing`.
                        for r in row.iter().take(*n as usize) {
                            put(out, &r.x.to_le_bytes());
                            put(out, &r.y.to_le_bytes());
                            put(out, &r.w.to_le_bytes());
                            put(out, &r.h.to_le_bytes());
                        }
                    }
                }
                Some(())
            }
            Title(t) => text(out, t, MAX_TITLE),
            StateSaved | Bye => Some(()),
        }
    }

    fn body_from(t: u8, p: &mut Cur<'_>) -> Result<Self, Decode> {
        use ToShell::{Bye, Frame, Hello, StateSaved, Surface, Title};
        match t {
            tag::HELLO => {
                let version = p.u32()?;
                let app_id = p.text("app id", MAX_ID)?;
                Ok(Hello { version, app_id })
            }
            tag::SURFACE => {
                let width = dim(p.u16()?, "width")?;
                let height = dim(p.u16()?, "height")?;
                let format = p.u8()?;
                if format != FORMAT_ARGB8888 {
                    return Err(Decode::Range {
                        what: "format",
                        got: format as u32,
                    });
                }
                let buffers = p.u8()?;
                if !(1..=MAX_BUFFERS).contains(&buffers) {
                    return Err(Decode::Range {
                        what: "buffer count",
                        got: buffers as u32,
                    });
                }
                Ok(Surface {
                    width,
                    height,
                    format,
                    buffers,
                    fds: Fds::new(),
                })
            }
            tag::FRAME => {
                let index = p.u8()?;
                let wants_frame = p.boolean()?;
                let damage = match p.u8()? {
                    tag::DAMAGE_FULL => Damage::Full,
                    tag::DAMAGE_PARTS => {
                        let n = p.u8()? as usize;
                        if n == 0 || n > MAX_DAMAGE {
                            return Err(Decode::Range {
                                what: "damage count",
                                got: n as u32,
                            });
                        }
                        let mut row = [Rect::default(); MAX_DAMAGE];
                        for r in row.iter_mut().take(n) {
                            *r = Rect {
                                x: p.u16()?,
                                y: p.u16()?,
                                w: p.u16()?,
                                h: p.u16()?,
                            };
                        }
                        Damage::Parts(row, n as u8)
                    }
                    other => {
                        return Err(Decode::Range {
                            what: "damage kind",
                            got: other as u32,
                        });
                    }
                };
                Ok(Frame {
                    index,
                    damage,
                    wants_frame,
                    fds: Fds::new(),
                })
            }
            tag::TITLE => Ok(Title(p.text("title", MAX_TITLE)?)),
            tag::STATE_SAVED => Ok(StateSaved),
            tag::BYE => Ok(Bye),
            other => Err(Decode::Tag(other)),
        }
    }
}

impl Wire for ToApp {
    fn tag(&self) -> u8 {
        use ToApp::{Background, Close, Configure, Foreground, FrameDone, Input};
        match self {
            Configure { .. } => tag::CONFIGURE,
            FrameDone { .. } => tag::FRAME_DONE,
            Input(_) => tag::INPUT,
            Background => tag::BACKGROUND,
            Foreground => tag::FOREGROUND,
            Close => tag::CLOSE,
        }
    }

    fn body(&self, out: &mut Vec<u8>) -> Option<()> {
        use ToApp::{Background, Close, Configure, Foreground, FrameDone, Input};
        match self {
            Configure {
                width,
                height,
                scale,
                focused,
            } => {
                put(out, &width.to_le_bytes());
                put(out, &height.to_le_bytes());
                out.push(*scale);
                boolean(out, *focused);
            }
            FrameDone { index, at_us } => {
                out.push(*index);
                put(out, &at_us.to_le_bytes());
            }
            Input(e) => {
                put(out, &e.type_.to_le_bytes());
                put(out, &e.code.to_le_bytes());
                put(out, &e.value.to_le_bytes());
                put(out, &e.at_us.to_le_bytes());
            }
            Background | Foreground | Close => {}
        }
        Some(())
    }

    fn body_from(t: u8, p: &mut Cur<'_>) -> Result<Self, Decode> {
        use ToApp::{Background, Close, Configure, Foreground, FrameDone, Input};
        match t {
            tag::CONFIGURE => {
                let width = dim(p.u16()?, "width")?;
                let height = dim(p.u16()?, "height")?;
                Ok(Configure {
                    width,
                    height,
                    scale: p.u8()?,
                    focused: p.boolean()?,
                })
            }
            tag::FRAME_DONE => Ok(FrameDone {
                index: p.u8()?,
                at_us: p.i64()?,
            }),
            tag::INPUT => Ok(Input(Event {
                type_: p.u16()?,
                code: p.u16()?,
                value: p.i32()?,
                at_us: p.i64()?,
            })),
            tag::BACKGROUND => Ok(Background),
            tag::FOREGROUND => Ok(Foreground),
            tag::CLOSE => Ok(Close),
            other => Err(Decode::Tag(other)),
        }
    }
}

/// The libc this socket needs, and the cmsg rules around it: three calls that make the pair and move a datagram, and two that
/// set a flag. Nothing else here is `unsafe`, and nothing here knows what a message means.
mod sys {
    use std::io::{self, ErrorKind};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::ptr;

    use super::MAX_FDS;

    /// Bytes of control buffer `sendmsg` and `recvmsg` are offered. `CMSG_SPACE(3 * 4)` is 32 on Linux — a 16-byte `cmsghdr`
    /// and a payload both aligned up — so this is double what the descriptors of one message need. Both calls check what they
    /// compute against it before writing to it, and `recvmsg` is handed the exact space rather than the whole array: that is
    /// what turns a fourth descriptor into `MSG_CTRUNC` rather than into a row the reader has to count for itself.
    const CTL: usize = 64;

    /// The space one message's worth of descriptors takes, or None when [`CTL`] does not cover it — the only bound that keeps
    /// the writes below inside the array. What a message may *carry* is [`MAX_FDS`], which [`send_msg`] checks.
    fn ctl_space(fds: usize) -> Option<usize> {
        let payload = (fds * std::mem::size_of::<RawFd>()) as u32;
        // SAFETY: CMSG_SPACE is arithmetic on its argument.
        let space = unsafe { libc::CMSG_SPACE(payload) } as usize;
        (space <= CTL).then_some(space)
    }

    /// One `SOCK_SEQPACKET` pair, both ends close-on-exec. The shell keeps one and gives the other to a child on purpose.
    pub fn socketpair() -> io::Result<(OwnedFd, OwnedFd)> {
        let mut sv = [0 as RawFd; 2];
        // SAFETY: socketpair writes both elements of the two-element array it is given a pointer to.
        if unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
                sv.as_mut_ptr(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both are new descriptors that nothing else owns.
        Ok(unsafe { (OwnedFd::from_raw_fd(sv[0]), OwnedFd::from_raw_fd(sv[1])) })
    }

    /// Add `set` to a descriptor's status flags, keeping the rest.
    pub fn set_nonblocking(fd: RawFd) -> io::Result<()> {
        // SAFETY: F_GETFL on a descriptor we own asks for its flags.
        let now = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if now < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: F_SETFL on that same descriptor, with its own flags widened.
        if unsafe { libc::fcntl(fd, libc::F_SETFL, now | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Mark one descriptor close-on-exec. The kernel does not do it for a descriptor received over `SCM_RIGHTS`: it hands over
    /// the number and nothing else, which a test below reads off this very call, and a shell that forks would otherwise pass
    /// one app's buffer into the next app that starts.
    fn set_cloexec(fd: RawFd) -> io::Result<()> {
        // SAFETY: F_SETFD takes one flag word for a descriptor we own.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Whether `fd` is open at all, and whether it is close-on-exec: what the tests check a received descriptor against.
    pub fn probe(fd: RawFd) -> (bool, bool) {
        // SAFETY: F_GETFD on a descriptor the caller names; a number the kernel does not know answers -1.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        (flags >= 0, flags & libc::FD_CLOEXEC != 0)
    }

    /// A second descriptor for `fd`, the lowest free one at or above `at_least`, already close-on-exec: how the app's end of a
    /// socketpair becomes the low number the child is told about.
    pub fn dup_from(fd: RawFd, at_least: RawFd) -> io::Result<OwnedFd> {
        // SAFETY: F_DUPFD_CLOEXEC either makes a new descriptor for this socket or fails; its third argument is a number, not
        // a pointer.
        let moved = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, at_least) };
        if moved < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `moved` is new, names nothing else, and is ours to close.
        Ok(unsafe { OwnedFd::from_raw_fd(moved) })
    }

    /// One datagram, with these descriptors beside it. `fds` is at most [`MAX_FDS`] long, which the [`super::Fds`] it comes
    /// from guarantees.
    pub fn send_msg(fd: RawFd, bytes: &[u8], fds: &[RawFd]) -> io::Result<()> {
        if fds.len() > MAX_FDS {
            return Err(io::Error::from_raw_os_error(libc::EOVERFLOW));
        }
        send_raw(fd, bytes, fds)
    }

    /// The same write with the count left to the caller: [`send_msg`] above once it has checked, and one test below that has to
    /// put more descriptors on a socket than the protocol allows, which is what a peer outside this codec does. Nothing here
    /// writes past [`CTL`]: [`ctl_space`] says when it would.
    fn send_raw(fd: RawFd, bytes: &[u8], fds: &[RawFd]) -> io::Result<()> {
        let space = ctl_space(fds.len()).ok_or(io::Error::from_raw_os_error(libc::EOVERFLOW))?;
        let mut iov = libc::iovec {
            iov_base: bytes.as_ptr() as *mut libc::c_void,
            iov_len: bytes.len(),
        };
        let mut ctl = [0u8; CTL];
        // SAFETY: an all-zero msghdr is a valid one.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        if !fds.is_empty() {
            msg.msg_control = ctl.as_mut_ptr() as *mut libc::c_void;
            msg.msg_controllen = space;
            // SAFETY: the control array is `space` bytes, which `CMSG_SPACE` says is room for one header plus exactly these
            // descriptor numbers; `fds` and `iov` outlive the sendmsg below.
            unsafe {
                let cm = libc::CMSG_FIRSTHDR(&raw const msg);
                if cm.is_null() {
                    return Err(io::Error::other("no room for a control header"));
                }
                // The bytes of the descriptor row, and only then its length: `CMSG_LEN` wants the payload size of the slice.
                let payload = std::mem::size_of_val(fds);
                (*cm).cmsg_level = libc::SOL_SOCKET;
                (*cm).cmsg_type = libc::SCM_RIGHTS;
                (*cm).cmsg_len = libc::CMSG_LEN(payload as u32) as _;
                ptr::copy_nonoverlapping(fds.as_ptr() as *const u8, libc::CMSG_DATA(cm), payload);
            }
        }
        // SAFETY: `msg` names the buffers above, all of them alive, and the kernel only reads them.
        if unsafe { libc::sendmsg(fd, &raw const msg, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The one place [`send_raw`] is reached with the bound unchecked, and only in a test: the reader's side of a fourth
    /// descriptor is otherwise unreachable from inside this codec, and it is the path that must not leak.
    #[cfg(test)]
    pub(super) fn send_past_the_bound(fd: RawFd, bytes: &[u8], fds: &[RawFd]) -> io::Result<()> {
        send_raw(fd, bytes, fds)
    }

    /// What a datagram says: None when nothing is waiting or a signal interrupted the read, `Err` for anything else, the other
    /// end being gone included. Every descriptor that came is put in `fds` before an error is returned, so the caller dropping
    /// `fds` is what closes them.
    pub fn recv_msg(fd: RawFd, buf: &mut [u8], fds: &mut super::Fds) -> io::Result<Option<usize>> {
        let space =
            ctl_space(MAX_FDS).ok_or_else(|| io::Error::other("the control space is too small"))?;
        let mut iov = libc::iovec {
            iov_base: buf.as_mut_ptr() as *mut libc::c_void,
            iov_len: buf.len(),
        };
        let mut ctl = [0u8; CTL];
        // SAFETY: an all-zero msghdr is a valid one.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = ctl.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = space;
        // SAFETY: recvmsg writes into `iov`'s buffer and `ctl`, both alive for the call, and through `msg` reads nothing else.
        let n = unsafe { libc::recvmsg(fd, &raw mut msg, 0) };
        let n = if n < 0 {
            let e = io::Error::last_os_error();
            match e.raw_os_error() {
                // Nothing waiting: the shell has other things to do. (EWOULDBLOCK is EAGAIN.)
                Some(libc::EAGAIN) => return Ok(None),
                // The poll loop is where a signal has to land anyway; nothing here loops.
                Some(libc::EINTR) => return Ok(None),
                _ => return Err(e),
            }
        } else if n == 0 {
            return Err(io::Error::new(
                ErrorKind::UnexpectedEof,
                "the app's end of the surface socket is closed",
            ));
        } else {
            n as usize
        };
        // SAFETY: `msg` names `ctl`, `space` bytes of it, from a recvmsg that has returned; the walk reads only inside those
        // bytes, and every descriptor it takes is owned by the row it returns, so an error below closes them again.
        let got = unsafe { read_fds(&raw const msg, space) }?;
        let ctrunc = msg.msg_flags & libc::MSG_CTRUNC != 0;
        let trunc = msg.msg_flags & libc::MSG_TRUNC != 0;
        for f in got {
            if fds.push(f).is_err() {
                // The row gave the descriptor back, and the Result drops it, which closes it: this is the only way out of
                // the loop with a descriptor not owned by anything.
                return Err(io::Error::other(
                    "a message came with more descriptors than the socket may carry",
                ));
            }
        }
        if ctrunc {
            return Err(io::Error::other(
                "a message came with more descriptors than the socket may carry",
            ));
        }
        if trunc {
            return Err(io::Error::other(
                "a message came longer than any message may be",
            ));
        }
        Ok(Some(n))
    }

    /// The descriptors in the control data of a received message, and nothing else: an unknown header, a length that is not a
    /// whole number of descriptors, or more than [`MAX_FDS`] of them is an error. The kernel has closed what did not fit the
    /// space it was given; what it did hand over is in the returned row, so whoever drops it closes it.
    ///
    /// # Safety
    ///
    /// `msg` must be a `msghdr` from a `recvmsg` that returned, whose `msg_control` names `space` readable bytes that are
    /// alive for this call.
    unsafe fn read_fds(msg: *const libc::msghdr, space: usize) -> io::Result<Vec<OwnedFd>> {
        let mut out = Vec::new();
        unsafe {
            let base = (*msg).msg_control.cast::<u8>();
            let limit = base.add(space);
            let mut cm = libc::CMSG_FIRSTHDR(msg);
            while !cm.is_null() {
                let head = std::mem::size_of::<libc::cmsghdr>();
                let body = libc::CMSG_LEN(0) as usize;
                let at = cm.cast::<u8>();
                if at < base || at.add(head) > limit {
                    return Err(io::Error::other(
                        "a control header runs outside the space it was given",
                    ));
                }
                let len = (*cm).cmsg_len as usize;
                if len < body || at.add(len) > limit {
                    return Err(io::Error::other(
                        "a control body runs outside the space it was given",
                    ));
                }
                if (*cm).cmsg_level != libc::SOL_SOCKET || (*cm).cmsg_type != libc::SCM_RIGHTS {
                    return Err(io::Error::other(
                        "a message came with control data this protocol does not name",
                    ));
                }
                let each = std::mem::size_of::<RawFd>();
                let bytes = len - body;
                if !bytes.is_multiple_of(each) {
                    return Err(io::Error::other(
                        "a descriptor row that is not a whole number of descriptors",
                    ));
                }
                let count = bytes / each;
                // The kernel installed every number in this row before it returned, so every one of them is ours to close —
                // the row that is too long included. Owned first, refused after, which is what makes the refusal cost nothing.
                let row = std::slice::from_raw_parts(libc::CMSG_DATA(cm) as *const RawFd, count);
                for &f in row {
                    // Owned from here: any later error closes it. And close-on-exec from here, because the shell is the one
                    // process that forks: what arrives must not survive into the next app's exec.
                    let owned = OwnedFd::from_raw_fd(f);
                    set_cloexec(owned.as_raw_fd())?;
                    out.push(owned);
                }
                if count > MAX_FDS {
                    return Err(io::Error::other(
                        "more descriptors than a message may carry",
                    ));
                }
                cm = libc::CMSG_NXTHDR(msg, cm);
            }
        }
        Ok(out)
    }
}

/// One end of one app's surface socket. The shell's end is non-blocking and waits for nobody: [`Link::send`] queues and
/// returns, [`Link::recv`] says when there is nothing to read.
#[derive(Debug)]
pub struct Link {
    fd: OwnedFd,
    outbox: VecDeque<ToApp>,
}

impl Link {
    /// Both ends at once: the shell's, ready to poll, and the app's as a descriptor to hand to a child. The app's end keeps
    /// close-on-exec — [`Link::for_child`] and the spawn path decide what the child keeps — and keeps blocking, which is the
    /// back-pressure a compositor wants: an app that paints faster than the shell reads waits, and the shell never does.
    pub fn pair() -> io::Result<(Link, OwnedFd)> {
        let (shell, app) = sys::socketpair()?;
        sys::set_nonblocking(shell.as_raw_fd())?;
        Ok((
            Link {
                fd: shell,
                outbox: VecDeque::new(),
            },
            app,
        ))
    }

    /// An end wrapped as it is: what the app side (`lucina_app`, and the tests below that stand in for an app) reads and
    /// writes. The descriptor's flags are left alone.
    pub fn from_fd(fd: OwnedFd) -> Link {
        Link {
            fd,
            outbox: VecDeque::new(),
        }
    }

    /// Move the app's end to the lowest free descriptor at or above [`APP_FD`], leaving it close-on-exec: the number it lands
    /// on is what the child is told, the way `apps.rs` moves the hold pipe to 3 and clears the flag in `pre_exec`.
    pub fn for_child(app: OwnedFd) -> io::Result<OwnedFd> {
        let moved = sys::dup_from(app.as_raw_fd(), APP_FD)?;
        // `app` still names the old number: dropping it closes that one and leaves `moved`, which refers to the same socket,
        // open. The lowest slot at or above `APP_FD` is what the kernel gives back, and the child is told that number.
        drop(app);
        Ok(moved)
    }

    /// The descriptor to `poll`: readable when a message waits, writable when [`Link::flush`] has something left to send.
    pub fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// How many messages wait for the socket. Non-zero means poll for write and call [`Link::flush`].
    pub fn outbox_len(&self) -> usize {
        self.outbox.len()
    }

    /// One message to the app: written now if the socket takes it, queued if not, and never waiting.
    pub fn send(&mut self, msg: ToApp) -> io::Result<()> {
        if self.outbox.is_empty() {
            match self.write(&msg) {
                Ok(()) => return Ok(()),
                // Not now: it queues, like everything else. An interrupted or refused write queues too rather than leaving the
                // socket half-written and the caller guessing.
                Err(e)
                    if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        self.stow(msg)
    }

    /// The socket is ready: send what the queue holds, in order, until it says stop. Returns how many went. A queue that never
    /// empties is an app that stopped reading, and the shell ends it.
    pub fn flush(&mut self) -> io::Result<usize> {
        let mut sent = 0;
        while let Some(front) = self.outbox.front() {
            match self.write(front) {
                Ok(()) => {
                    self.outbox.pop_front();
                    sent += 1;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }
        Ok(sent)
    }

    /// What an app sends, with the buffers beside it: `surface` and `frame` carry descriptors, and nothing else may. The shell
    /// never calls this — its messages carry none — but the codec and the socket rules are one thing on both ends, and slice W
    /// needs an app that really talks, as do the tests below.
    ///
    /// No outbox here, on purpose: this is the app's side, and an app that cannot send waits, which is the back-pressure that
    /// keeps a compositor from queueing frames it will never show.
    pub fn send_from_app(&mut self, msg: &ToShell, fds: &Fds) -> io::Result<()> {
        match msg {
            ToShell::Surface { .. } | ToShell::Frame { .. } => {}
            _ if !fds.is_empty() => {
                return Err(io::Error::other(
                    "a message that names no buffers came with some",
                ));
            }
            _ => {}
        }
        let mut out = Vec::with_capacity(MAX_MSG);
        if msg.encode(&mut out).is_none() {
            return Err(io::Error::other(
                "the message is bigger than one frame may be",
            ));
        }
        sys::send_msg(self.fd.as_raw_fd(), &out, &fds.raw())
    }

    /// One message from the app, or None when nothing waits. A message that does not decode — truncated, lying about a length,
    /// carrying descriptors it may not — is an error naming itself, and closes only what came with it: the app keeps its
    /// socket, and the shell's loop goes on to the next descriptor it does have to read.
    pub fn recv(&mut self) -> io::Result<Option<ToShell>> {
        let mut buf = [0u8; MAX_MSG];
        let mut fds = Fds::new();
        let n = match sys::recv_msg(self.fd.as_raw_fd(), &mut buf, &mut fds) {
            Ok(Some(n)) => n,
            Ok(None) => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut msg = ToShell::decode(&buf[..n])?;
        let want = match &msg {
            // A surface promises the row of buffers the kernel carried: the count in the frame and the number that came have
            // to agree, or the app described a surface it did not send.
            ToShell::Surface { buffers, .. } => *buffers as usize,
            // A frame may carry a fence or none, so no number is checked: only the cap, which `Fds` cannot pass.
            ToShell::Frame { .. } => fds.len(),
            // Nothing else names a descriptor, so anything that came is a lie.
            _ => 0,
        };
        if want != fds.len() {
            return Err(io::Error::other(
                "a message came with a different number of descriptors than it names",
            ));
        }
        match &mut msg {
            ToShell::Surface { fds: slot, .. } | ToShell::Frame { fds: slot, .. } => *slot = fds,
            _ => {}
        }
        Ok(Some(msg))
    }

    /// The app's side of a read: what the shell sent. The same rules, the other way up, and here for the app side and the
    /// tests; an app's end takes no descriptors from the shell, so none may come.
    pub fn recv_from_shell(&mut self) -> io::Result<Option<ToApp>> {
        let mut buf = [0u8; MAX_MSG];
        let mut none = Fds::new();
        let n = match sys::recv_msg(self.fd.as_raw_fd(), &mut buf, &mut none) {
            Ok(Some(n)) => n,
            Ok(None) => return Ok(None),
            Err(e) => return Err(e),
        };
        if !none.is_empty() {
            return Err(io::Error::other(
                "the shell sent a message that carries no descriptors some descriptors",
            ));
        }
        Ok(Some(ToApp::decode(&buf[..n])?))
    }

    /// One frame straight to the socket.
    fn write(&self, msg: &ToApp) -> io::Result<()> {
        let mut out = Vec::with_capacity(MAX_MSG);
        if msg.encode(&mut out).is_none() {
            return Err(io::Error::other(
                "the message is bigger than one frame may be",
            ));
        }
        sys::send_msg(self.fd.as_raw_fd(), &out, &[])
    }

    /// The socket was not ready, so make room by the module's policy or lose this message.
    fn stow(&mut self, msg: ToApp) -> io::Result<()> {
        if self.outbox.len() < OUTBOX_MAX {
            self.outbox.push_back(msg);
            return Ok(());
        }
        if msg == ToApp::Close {
            return Err(io::Error::from_raw_os_error(libc::EOVERFLOW));
        }
        if let Some(i) = self.outbox.iter().position(|o| msg.supersedes(o)) {
            self.outbox[i] = msg;
            return Ok(());
        }
        if let Some(i) = self.outbox.iter().position(ToApp::is_sample) {
            self.outbox.remove(i);
            self.outbox.push_back(msg);
            return Ok(());
        }
        // Nothing to make room with: the queue holds only messages that must not be lost, and this one is worth waiting for.
        // A `close` behind a queued `close`, or a state message behind a row of them, says nothing new.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::sync::{Mutex, MutexGuard};

    /// One message of each kind, across its bounds. The extremes are here because a codec's bugs live at them: 1 and
    /// [`MAX_DIM`] for a side, empty and full for a string, every damage count, the widest signed values.
    fn shell_cases() -> Vec<ToShell> {
        let four = Damage::parts(&[
            Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            Rect {
                x: 10,
                y: 20,
                w: 30,
                h: 40,
            },
            Rect {
                x: u16::MAX,
                y: 0,
                w: 0,
                h: u16::MAX,
            },
            Rect {
                x: 1,
                y: 2,
                w: 3,
                h: 4,
            },
        ])
        .expect("four rectangles are a row");
        vec![
            ToShell::Hello {
                version: PROTO_VERSION,
                app_id: "video".into(),
            },
            ToShell::Hello {
                version: u32::MAX,
                app_id: String::new(),
            },
            ToShell::Hello {
                version: 0,
                app_id: "a".repeat(MAX_ID),
            },
            ToShell::Surface {
                width: 1,
                height: 1,
                format: FORMAT_ARGB8888,
                buffers: 1,
                fds: Fds::new(),
            },
            ToShell::Surface {
                width: MAX_DIM,
                height: MAX_DIM,
                format: FORMAT_ARGB8888,
                buffers: 2,
                fds: Fds::new(),
            },
            ToShell::Frame {
                index: 0,
                damage: Damage::Full,
                wants_frame: false,
                fds: Fds::new(),
            },
            ToShell::Frame {
                index: u8::MAX,
                damage: Damage::parts(&[Rect {
                    x: 1,
                    y: 2,
                    w: 3,
                    h: 4,
                }])
                .expect("one rectangle"),
                wants_frame: true,
                fds: Fds::new(),
            },
            ToShell::Frame {
                index: 1,
                damage: four,
                wants_frame: true,
                fds: Fds::new(),
            },
            ToShell::Title(String::new()),
            ToShell::Title("Notes — Untitled".into()),
            ToShell::Title("é".repeat(MAX_TITLE / 2)),
            ToShell::StateSaved,
            ToShell::Bye,
        ]
    }

    fn app_cases() -> Vec<ToApp> {
        vec![
            ToApp::Configure {
                width: 1,
                height: 1,
                scale: 1,
                focused: false,
            },
            ToApp::Configure {
                width: MAX_DIM,
                height: 3,
                scale: u8::MAX,
                focused: true,
            },
            ToApp::FrameDone {
                index: 1,
                at_us: i64::MIN,
            },
            ToApp::FrameDone {
                index: u8::MAX,
                at_us: i64::MAX,
            },
            ToApp::Input(Event {
                type_: 0,
                code: 0,
                value: 0,
                at_us: 0,
            }),
            ToApp::Input(Event {
                type_: u16::MAX,
                code: u16::MAX,
                value: i32::MIN,
                at_us: -1,
            }),
            ToApp::Background,
            ToApp::Foreground,
            ToApp::Close,
        ]
    }

    /// Descriptors are process-global and these tests are threads of one process: another test opening a socket between two
    /// counts would read as a leak. Every test that counts or places descriptors holds this.
    static FDS: Mutex<()> = Mutex::new(());

    fn quiet<T>(f: impl FnOnce() -> T) -> T {
        let guard: MutexGuard<'_, ()> = FDS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let out = f();
        drop(guard);
        out
    }

    /// How many descriptors this process has open right now.
    fn open_fds() -> usize {
        std::fs::read_dir("/proc/self/fd")
            .map(|d| d.count())
            .expect("/proc/self/fd is readable")
    }

    /// A `Link` whose queue holds `held`, without going near the socket. The peer end stays open and unread, and a non-empty
    /// queue means [`Link::send`] never writes: it goes straight to the policy, which is what these tests are about.
    fn link_holding(held: impl IntoIterator<Item = ToApp>) -> Link {
        let (link, _peer) = Link::pair().expect("socketpair");
        Link {
            fd: link.fd,
            outbox: held.into_iter().collect(),
        }
    }

    /// A queue of `n` messages, `at` of them `old` and the rest samples that say which one they are.
    fn full_queue(n: usize, at: usize, old: ToApp) -> Vec<ToApp> {
        (0..n as i32)
            .map(|i| if i as usize == at { old } else { sample(i) })
            .collect()
    }

    /// An `input` record that names itself, so a queue's order is readable.
    fn sample(i: i32) -> ToApp {
        ToApp::Input(Event {
            type_: 1,
            code: 28,
            value: i,
            at_us: i as i64,
        })
    }

    /// A `surface` frame: header, then the two sides, the format and the buffer count.
    fn surface(width: u16, height: u16, format: u8, buffers: u8) -> Vec<u8> {
        let mut v = vec![WIRE_VERSION, tag::SURFACE, 6, 0];
        v.extend(width.to_le_bytes());
        v.extend(height.to_le_bytes());
        v.extend([format, buffers]);
        v
    }

    /// A `configure` frame, same shape, so both directions' bounds are tested in the same words.
    fn configure(width: u16, height: u16) -> Vec<u8> {
        let mut v = vec![WIRE_VERSION, tag::CONFIGURE, 6, 0];
        v.extend(width.to_le_bytes());
        v.extend(height.to_le_bytes());
        v.extend([1, 1]);
        v
    }

    /// A `frame` frame laid out as the codec lays it out: index, flag, damage kind, and for a row of rectangles the count and
    /// that many zeroed rectangles. The header's length says exactly what follows, so a count out of range is the parser
    /// refusing a lie, not running off the end.
    fn frame(kind: u8, n: u8, flag: u8) -> Vec<u8> {
        let payload = if kind == tag::DAMAGE_PARTS {
            4 + n as usize * 8
        } else {
            3
        };
        let mut v = vec![
            WIRE_VERSION,
            tag::FRAME,
            payload as u8,
            (payload >> 8) as u8,
            0,
            flag,
            kind,
        ];
        if kind == tag::DAMAGE_PARTS {
            v.push(n);
            v.extend(std::iter::repeat_n(0u8, n as usize * 8));
        }
        v
    }

    /// Every app-to-shell message comes back from its own bytes, and none of them is bigger than a frame may be.
    #[test]
    fn a_shell_message_round_trips() {
        for m in shell_cases() {
            let bytes = m.to_vec().expect("a message that fits a frame");
            assert!(bytes.len() <= MAX_MSG, "{m:?} in {} bytes", bytes.len());
            assert_eq!(bytes[0], WIRE_VERSION, "the version leads every frame");
            assert_eq!(ToShell::decode(&bytes), Ok(m));
        }
    }

    #[test]
    fn an_app_message_round_trips() {
        for m in app_cases() {
            let bytes = m.to_vec().expect("a message that fits a frame");
            assert!(bytes.len() <= MAX_MSG, "{m:?} in {} bytes", bytes.len());
            assert_eq!(ToApp::decode(&bytes), Ok(m), "{m:?}");
        }
    }

    /// Descriptors travel beside the frame, in the kernel's hands and not in the payload: the frame of a `surface` that carries
    /// two buffers is byte for byte the frame of one that carries none.
    #[test]
    fn the_descriptors_are_not_counted_in_the_frame() {
        quiet(|| {
            let (shell_a, app_a) = Link::pair().expect("socketpair");
            let (shell_b, app_b) = Link::pair().expect("socketpair");
            let mut with = Fds::new();
            with.push(app_a).expect("a free slot");
            with.push(app_b).expect("a free slot");
            assert_eq!(with.len(), 2);
            let sided = |fds| ToShell::Surface {
                width: 8,
                height: 8,
                format: FORMAT_ARGB8888,
                buffers: 2,
                fds,
            };
            assert_eq!(
                sided(with).to_vec().expect("a frame"),
                sided(Fds::new()).to_vec().expect("a frame"),
                "the frame says two buffers and nothing about the descriptors"
            );
            assert_eq!(sided(Fds::new()).to_vec().expect("a frame").len(), 4 + 6);
            drop((shell_a, shell_b));
        });
    }

    /// Every prefix of every frame is refused: a message that stops half way is never read as one that ended.
    #[test]
    fn every_truncated_frame_is_refused() {
        for m in shell_cases() {
            let bytes = m.to_vec().expect("a frame");
            for cut in 0..bytes.len() {
                assert!(
                    ToShell::decode(&bytes[..cut]).is_err(),
                    "{m:?} cut to {cut} read as {:?}",
                    ToShell::decode(&bytes[..cut]).ok()
                );
            }
        }
        for m in app_cases() {
            let bytes = m.to_vec().expect("a frame");
            for cut in 0..bytes.len() {
                assert!(ToApp::decode(&bytes[..cut]).is_err(), "{m:?} cut to {cut}");
            }
        }
    }

    /// A byte flipped on the wire is refused, or means another message: never a panic, never a read past the frame, never a
    /// message outside its bounds, and what survives is what its own bytes say again.
    #[test]
    fn a_flipped_byte_never_panics() {
        for m in shell_cases() {
            let base = m.to_vec().expect("a frame");
            for i in 0..base.len() {
                for bit in 0..8 {
                    let mut b = base.clone();
                    b[i] ^= 1u8 << bit;
                    if let Ok(got) = ToShell::decode(&b) {
                        match &got {
                            ToShell::Title(t) => assert!(t.len() <= MAX_TITLE),
                            ToShell::Surface { width, height, .. } => {
                                assert!(*width >= 1 && *width <= MAX_DIM);
                                assert!(*height >= 1 && *height <= MAX_DIM);
                            }
                            ToShell::Frame { damage, .. } => assert!(damage.len() <= MAX_DAMAGE),
                            _ => {}
                        }
                        assert_eq!(ToShell::decode(&got.to_vec().expect("a frame")), Ok(got));
                    }
                }
            }
        }
    }

    /// An xorshift with a fixed seed, so a failure here reproduces: 2000 rounds of plain noise, and of noise behind a header
    /// that names a version, a tag and a length, which is what gets the payload parsers running.
    fn xorshift(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    #[test]
    fn noise_never_panics_and_never_escapes_its_bounds() {
        let mut seed = 0x2026_1117_u64;
        for _ in 0..2000 {
            let n = xorshift(&mut seed) as usize % (MAX_MSG + 40);
            let noise: Vec<u8> = (0..n).map(|_| (xorshift(&mut seed) >> 24) as u8).collect();
            let _ = ToShell::decode(&noise);
            let _ = ToApp::decode(&noise);

            let mut framed = vec![
                WIRE_VERSION,
                (xorshift(&mut seed) % 16) as u8,
                (xorshift(&mut seed) & 0xff) as u8,
                (xorshift(&mut seed) >> 8) as u8,
            ];
            let extra = xorshift(&mut seed) as usize % 40;
            framed.extend((0..extra).map(|_| (xorshift(&mut seed) >> 24) as u8));
            if let Ok(m) = ToShell::decode(&framed) {
                match &m {
                    ToShell::Title(t) => assert!(t.len() <= MAX_TITLE, "{m:?}"),
                    ToShell::Hello { app_id, .. } => assert!(app_id.len() <= MAX_ID, "{m:?}"),
                    ToShell::Surface { width, height, .. } => {
                        assert!(*width >= 1 && *width <= MAX_DIM, "{m:?}");
                        assert!(*height >= 1 && *height <= MAX_DIM, "{m:?}");
                    }
                    ToShell::Frame { damage, .. } => assert!(damage.len() <= MAX_DAMAGE, "{m:?}"),
                    _ => {}
                }
                assert_eq!(ToShell::decode(&m.to_vec().expect("a frame")), Ok(m));
            }
            if let Ok(m) = ToApp::decode(&framed) {
                assert_eq!(ToApp::decode(&m.to_vec().expect("a frame")), Ok(m));
            }
        }
    }

    /// A title past the 64 bytes section 5a allows: the encoder will not build it, and the decoder refuses it before reading
    /// its bytes. The app id has the same bound and the same two answers.
    #[test]
    fn an_oversize_title_is_refused() {
        let long = "x".repeat(MAX_TITLE + 1);
        assert_eq!(
            ToShell::Title(long.clone()).to_vec(),
            None,
            "the encoder must not emit a title the wire forbids"
        );
        let mut bytes = vec![WIRE_VERSION, tag::TITLE, 2 + 65, 0, 65, 0];
        bytes.extend(long.as_bytes());
        assert_eq!(bytes.len(), 4 + 67, "the header promises what is there");
        assert_eq!(
            ToShell::decode(&bytes),
            Err(Decode::TooLong {
                what: "title",
                max: MAX_TITLE,
                got: MAX_TITLE + 1
            })
        );

        let id_len = MAX_ID + 1;
        let mut hello = vec![WIRE_VERSION, tag::HELLO];
        hello.extend((4 + 2 + id_len as u16).to_le_bytes());
        hello.extend(PROTO_VERSION.to_le_bytes());
        hello.extend((id_len as u16).to_le_bytes());
        hello.extend("y".repeat(id_len).into_bytes());
        assert_eq!(
            ToShell::decode(&hello),
            Err(Decode::TooLong {
                what: "app id",
                max: MAX_ID,
                got: MAX_ID + 1
            })
        );
        assert_eq!(
            ToShell::Hello {
                version: 1,
                app_id: "z".repeat(MAX_ID + 1)
            }
            .to_vec(),
            None
        );

        // And the tool a sender uses instead: cut to the bound, on a character boundary, never inside one. Three bytes is
        // exactly `hé` whole, four takes the next `l` with it, and two cuts inside the é, so it backs off to the `h`.
        assert_eq!(clamp_str("héllo wörld", 3), "hé");
        assert_eq!(clamp_str("héllo wörld", 4), "hél");
        assert_eq!(clamp_str("héllo wörld", 2), "h");
        assert_eq!(clamp_str("hi", 4), "hi");
        assert_eq!(clamp_str("", 4), "");
        assert_eq!(
            clamp_str("日本", 2),
            "",
            "one character does not fit two bytes"
        );
        assert_eq!(clamp_str("日本", 3), "日");
    }

    /// A title whose bytes are not UTF-8 is refused, and so are bytes left over after the payload the header counted.
    #[test]
    fn a_title_that_is_not_utf8_is_refused() {
        let mut bytes = vec![WIRE_VERSION, tag::TITLE, 3, 0, 1, 0, 0xff];
        assert_eq!(ToShell::decode(&bytes), Err(Decode::Utf8 { what: "title" }));
        bytes.push(0);
        assert_eq!(ToShell::decode(&bytes), Err(Decode::Trailing));
    }

    /// The header is checked before the payload: another version, a tag this direction does not define, a length past any
    /// frame, a payload that does not fill the frame.
    #[test]
    fn a_header_that_means_nothing_is_refused() {
        assert_eq!(ToShell::decode(&[]), Err(Decode::TooShort));
        assert_eq!(ToShell::decode(&[WIRE_VERSION]), Err(Decode::TooShort));
        assert_eq!(ToShell::decode(&[2, 6, 0, 0]), Err(Decode::Version(2)));
        assert_eq!(
            ToShell::decode(&[WIRE_VERSION, 0, 0, 0]),
            Err(Decode::Tag(0))
        );
        assert_eq!(
            ToShell::decode(&[WIRE_VERSION, tag::CLOSE, 0, 0]),
            Err(Decode::Tag(tag::CLOSE)),
            "the shell's tags are not the app's"
        );
        assert_eq!(
            ToApp::decode(&[WIRE_VERSION, tag::BYE, 0, 0]),
            Err(Decode::Tag(tag::BYE)),
            "nor the other way about"
        );
        assert_eq!(
            ToShell::decode(&[WIRE_VERSION, tag::BYE, 1, 0]),
            Err(Decode::TooShort)
        );
        assert_eq!(
            ToShell::decode(&[WIRE_VERSION, tag::BYE, 0, 0, 9]),
            Err(Decode::Trailing)
        );
        assert_eq!(
            ToShell::decode(&[WIRE_VERSION, tag::BYE, 0xff, 0xff]),
            Err(Decode::Len(u16::MAX)),
            "a length past any frame"
        );
        // The framing's version byte is the protocol's, and one number.
        assert_eq!(u32::from(WIRE_VERSION), PROTO_VERSION);
    }

    /// Every field this protocol bounds, answered out of its bounds, in both directions.
    #[test]
    fn an_out_of_range_field_is_refused() {
        let top = surface(MAX_DIM, MAX_DIM, FORMAT_ARGB8888, 2);
        assert!(
            ToShell::decode(&top).is_ok(),
            "{MAX_DIM} is the bound, not past it"
        );
        assert!(ToApp::decode(&configure(MAX_DIM, 1)).is_ok());
        for (bytes, what, got) in [
            (surface(0, 1, FORMAT_ARGB8888, 1), "width", 0u32),
            (surface(1, 0, FORMAT_ARGB8888, 1), "height", 0),
            (
                surface(MAX_DIM + 1, 1, FORMAT_ARGB8888, 1),
                "width",
                u32::from(MAX_DIM) + 1,
            ),
            (
                surface(1, MAX_DIM + 1, FORMAT_ARGB8888, 1),
                "height",
                u32::from(MAX_DIM) + 1,
            ),
            (configure(0, 1), "width", 0),
            (configure(1, MAX_DIM + 1), "height", u32::from(MAX_DIM) + 1),
        ] {
            let decoded = if bytes[1] == tag::SURFACE {
                ToShell::decode(&bytes).err()
            } else {
                ToApp::decode(&bytes).err()
            };
            assert_eq!(
                decoded,
                Some(Decode::Range { what, got }),
                "tag {} of {bytes:?}",
                bytes[1]
            );
        }
        assert_eq!(
            ToShell::decode(&surface(1, 1, 1, 1)).err(),
            Some(Decode::Range {
                what: "format",
                got: 1
            }),
            "only ARGB8888 today"
        );
        for bad in [0u8, 3, 4, u8::MAX] {
            assert_eq!(
                ToShell::decode(&surface(1, 1, FORMAT_ARGB8888, bad)).err(),
                Some(Decode::Range {
                    what: "buffer count",
                    got: bad as u32
                }),
                "{bad} buffers is neither one nor two, whatever the socket may carry"
            );
        }
        for (kind, n, want) in [
            (tag::DAMAGE_PARTS, 0u8, "damage count"),
            (tag::DAMAGE_PARTS, 5, "damage count"),
            (tag::DAMAGE_PARTS, 30, "damage count"),
            (2, 0, "damage kind"),
        ] {
            let got = ToShell::decode(&frame(kind, n, 1)).err();
            assert!(
                matches!(&got, Some(Decode::Range { what, .. }) if *what == want),
                "kind {kind} with {n}: {got:?}"
            );
        }
        assert!(
            frame(tag::DAMAGE_PARTS, 30, 1).len() <= MAX_MSG,
            "a row of thirty is a lie a frame can still carry, so the parser has to see it"
        );
        assert_eq!(
            ToShell::decode(&frame(tag::DAMAGE_FULL, 0, 2)).err(),
            Some(Decode::Range {
                what: "flag",
                got: 2
            }),
            "a flag is 0 or 1"
        );
        assert_eq!(
            ToShell::decode(&frame(tag::DAMAGE_FULL, 0, 1)),
            Ok(ToShell::Frame {
                index: 0,
                damage: Damage::Full,
                wants_frame: true,
                fds: Fds::new(),
            })
        );
        // A `hello` naming another protocol still decodes: the shell answers it, and cannot do that if the codec refuses it.
        assert_eq!(
            ToShell::decode(&{
                let mut v = vec![WIRE_VERSION, tag::HELLO, 4 + 2 + 1, 0];
                v.extend(7u32.to_le_bytes());
                v.extend(1u16.to_le_bytes());
                v.push(b'b');
                v
            }),
            Ok(ToShell::Hello {
                version: 7,
                app_id: "b".into()
            })
        );
    }

    /// `Damage::parts` is the sender's half of the same rule: none, or more than four, is not a row. The padding is zeros, so
    /// two `Damage`s naming the same rectangles compare equal, which is what the round trips above rely on.
    #[test]
    fn a_damage_row_is_between_one_and_four() {
        assert_eq!(Damage::parts(&[]), None);
        let one = Rect {
            x: 1,
            y: 2,
            w: 3,
            h: 4,
        };
        let d = Damage::parts(&[one]).expect("one rectangle");
        assert_eq!(d.len(), 1);
        assert_eq!(d.rects()[0], one);
        assert_eq!(d.rects()[1], Rect::default(), "the padding is zeros");
        assert_eq!(
            Damage::parts(&[one; MAX_DAMAGE]),
            Damage::parts(&[one, one, one, one]),
            "the same row, however it was written"
        );
        assert_eq!(
            Damage::parts(&[one; MAX_DAMAGE + 1]),
            None,
            "and a fifth is refused, not added"
        );
        assert!(Damage::Full.is_empty());
        assert_eq!(Damage::Full.len(), 0);
        assert!(Damage::Full.rects().is_empty());
    }

    /// The shell writes a `configure`; the app's end reads it off the real socket. The app's end is blocking by design (that is
    /// the back-pressure the module doc names), so the test widens it to read once more and hear that nothing else waits.
    #[test]
    fn a_configure_reaches_the_app_end() {
        quiet(|| {
            let (mut shell, app) = Link::pair().expect("socketpair");
            let m = ToApp::Configure {
                width: 2880,
                height: 1800,
                scale: 2,
                focused: true,
            };
            shell.send(m).expect("the socket takes it");
            assert_eq!(shell.outbox_len(), 0, "written straight through");
            let mut app = Link::from_fd(app);
            sys::set_nonblocking(app.fd()).expect("the app's end reads without waiting");
            assert_eq!(app.recv_from_shell().expect("a message"), Some(m));
            assert_eq!(app.recv_from_shell().expect("nothing else"), None);
        });
    }

    /// An app's `hello`, sent the way an app sends it, decoded by the shell. And what waits on a read that has nothing to read:
    /// `Ok(None)`, not a wait and not an error. A message that names no buffers is refused its descriptors on the sender's
    /// side too, so nothing reaches the socket to be read back as a lie.
    #[test]
    fn a_hello_from_the_app_is_decoded() {
        quiet(|| {
            let (mut shell, app) = Link::pair().expect("socketpair");
            assert_eq!(shell.recv().expect("nothing yet"), None);
            let mut app = Link::from_fd(app);
            let hello = ToShell::Hello {
                version: PROTO_VERSION,
                app_id: "player".into(),
            };
            // A message that names no buffers is refused them on the sender's side, so no lie ever reaches the socket.
            let mut one = Fds::new();
            one.push(File::open("/dev/null").expect("/dev/null").into())
                .expect("a free slot");
            assert!(
                app.send_from_app(&hello, &one).is_err(),
                "a hello carries no buffer"
            );
            assert_eq!(shell.recv().expect("nothing after the refusal"), None);
            app.send_from_app(&hello, &Fds::new())
                .expect("the socket takes it");
            assert_eq!(shell.recv().expect("a message"), Some(hello));
        });
    }

    /// A `surface` with two buffers: the shell gets two descriptors, each its own name, each live and close-on-exec, and what
    /// came through one of them is the bytes the app wrote through the other.
    #[test]
    fn buffer_descriptors_arrive_valid_and_close_on_exec() {
        quiet(|| {
            let (mut shell, app_fd) = Link::pair().expect("socketpair");
            // Two pipes stand in for two buffers: one end of each goes to the shell, the other stays here and keeps the pixels.
            let (a_read, mut a_write) = std::io::pipe().expect("a pipe to stand in for a buffer");
            let (b_read, b_write) = std::io::pipe().expect("a second one");
            a_write
                .write_all(b"pixels")
                .expect("the buffer has bytes in it");
            let mut sent = Fds::new();
            sent.push(a_read.into()).expect("a free slot");
            sent.push(b_write.into()).expect("a free slot");
            assert_eq!(sent.len(), 2);
            let mine = [
                sent.get(0).expect("the first").as_raw_fd(),
                sent.get(1).expect("the second").as_raw_fd(),
            ];

            let mut app = Link::from_fd(app_fd);
            let before = open_fds();
            app.send_from_app(
                &ToShell::Surface {
                    width: 32,
                    height: 32,
                    format: FORMAT_ARGB8888,
                    buffers: 2,
                    fds: Fds::new(),
                },
                &sent,
            )
            .expect("the app sends its buffers");

            let mut got = shell.recv().expect("a message").expect("one message");
            let (width, height, fds) = match &mut got {
                ToShell::Surface {
                    width, height, fds, ..
                } => (*width, *height, fds),
                other => panic!("a surface was sent, {other:?} came"),
            };
            assert_eq!((width, height), (32, 32));
            assert_eq!(fds.len(), 2, "both buffers came");
            let first = fds.get(0).expect("the first").as_raw_fd();
            let second = fds.get(1).expect("the second").as_raw_fd();
            assert_ne!(first, second, "two names, not one twice");
            assert!(
                !mine.contains(&first) && !mine.contains(&second),
                "the receiver's numbers are its own, not the sender's"
            );
            assert!(
                first < 4096 && second < 4096,
                "numbers this side of the bound `probe` reads, so a refusal cannot look like an open descriptor"
            );
            assert_eq!(
                sys::probe(first),
                (true, true),
                "open, and close-on-exec: the kernel does not do the second for a received descriptor, so we do"
            );
            assert_eq!(sys::probe(second), (true, true));
            let mut taken = File::from(fds.take(0).expect("the first buffer"));
            let mut buf = [0u8; 6];
            taken
                .read_exact(&mut buf)
                .expect("the pixels the app wrote came out of the descriptor the shell received");
            assert_eq!(&buf, b"pixels");
            drop(taken);
            drop(got);
            assert_eq!(
                open_fds(),
                before,
                "both received descriptors were closed with the message, and nothing else"
            );
            drop((sent, a_write, b_read));
        });
    }

    /// More descriptors than a message may carry. Only a peer outside this codec can do that — [`sys::send_msg`] refuses it,
    /// which is checked here too — so the test reaches past that bound on purpose, and what it proves is the part that matters:
    /// the refusal costs no descriptor. Whatever the kernel installed for us to close is closed on the error path, and the
    /// socket goes on carrying messages.
    #[test]
    fn more_descriptors_than_a_message_may_carry_are_refused_without_leak() {
        quiet(|| {
            let four: Vec<File> = (0..4)
                .map(|_| File::open("/dev/null").expect("/dev/null"))
                .collect();
            let raws: Vec<RawFd> = four.iter().map(AsRawFd::as_raw_fd).collect();
            let mut frame_of_three = Vec::new();
            ToShell::Frame {
                index: 0,
                damage: Damage::Full,
                wants_frame: false,
                fds: Fds::new(),
            }
            .encode(&mut frame_of_three)
            .expect("a frame");
            let (mut shell, app) = Link::pair().expect("socketpair");

            // The bound on the sending side: this codec will not put four on a socket.
            assert_eq!(
                sys::send_msg(app.as_raw_fd(), &frame_of_three, &raws)
                    .expect_err("four is refused")
                    .raw_os_error(),
                Some(libc::EOVERFLOW)
            );
            // And on the queueing side, where a fourth has nowhere to be kept: `push` gives it back, and giving it back drops
            // it, which closes it.
            let mut row = Fds::new();
            for _ in 0..MAX_FDS + 1 {
                let f: OwnedFd = File::open("/dev/null").expect("/dev/null").into();
                if row.push(f).is_err() {
                    break;
                }
            }
            assert_eq!(row.len(), MAX_FDS, "three of four, no more");
            drop(row);

            let before = open_fds();
            sys::send_past_the_bound(app.as_raw_fd(), &frame_of_three, &raws)
                .expect("four went out, past the bound");
            let e = shell.recv().expect_err("four descriptors is an error");
            assert_eq!(open_fds(), before, "and they cost a descriptor apiece: {e}");

            // The offending message cost the socket nothing either: the next one arrives.
            let mut app = Link::from_fd(app);
            app.send_from_app(&ToShell::Bye, &Fds::new())
                .expect("the app still has its socket");
            assert_eq!(shell.recv().expect("the next message"), Some(ToShell::Bye));
            drop(four);
        });
    }

    /// A message that says nothing about descriptors and arrives with some, and a `surface` whose frame says two buffers while
    /// the socket carried one: both refused, both closing what came with them, and the socket still talking afterwards.
    #[test]
    fn a_message_that_lies_about_descriptors_is_refused_and_its_cost_paid() {
        quiet(|| {
            let (mut shell, app) = Link::pair().expect("socketpair");
            let held = File::open("/dev/null").expect("/dev/null");

            let mut out = Vec::new();
            ToShell::Title("a lie".into())
                .encode(&mut out)
                .expect("a frame");
            sys::send_msg(app.as_raw_fd(), &out, &[held.as_raw_fd()]).expect("it went");
            let before = open_fds();
            let e = shell.recv().expect_err("a title carries nothing");
            assert_eq!(
                open_fds(),
                before,
                "the descriptor that came with the title was closed with the refusal: {e}"
            );

            let mut out = Vec::new();
            ToShell::Surface {
                width: 4,
                height: 4,
                format: FORMAT_ARGB8888,
                buffers: 2,
                fds: Fds::new(),
            }
            .encode(&mut out)
            .expect("a frame");
            sys::send_msg(app.as_raw_fd(), &out, &[held.as_raw_fd()])
                .expect("one descriptor for a surface that names two");
            let before = open_fds();
            let e = shell.recv().expect_err("one is not two");
            assert_eq!(
                open_fds(),
                before,
                "the descriptor of the surface that lied was closed with the refusal: {e}"
            );

            let mut app = Link::from_fd(app);
            app.send_from_app(&ToShell::StateSaved, &Fds::new())
                .expect("still talking");
            assert_eq!(
                shell.recv().expect("the next message"),
                Some(ToShell::StateSaved)
            );
            drop(held);
        });
    }

    /// The app's end going away is not a message and not a hang: it is one error, said the same way every time, so a loop that
    /// reads this descriptor again is the caller's own business, not this module's surprise.
    #[test]
    fn a_closed_app_end_reads_as_end_of_file() {
        quiet(|| {
            let (mut shell, app) = Link::pair().expect("socketpair");
            drop(app);
            let e = shell.recv().expect_err("the app is gone");
            assert_eq!(e.kind(), ErrorKind::UnexpectedEof, "{e}");
            assert_eq!(
                shell.recv().expect_err("and stays gone").kind(),
                ErrorKind::UnexpectedEof,
                "a second read says the same thing, rather than reading as no message forever"
            );
        });
    }

    /// A full outbox and a sample: the oldest one of them gives its place to the newest, and the count never moves.
    #[test]
    fn a_full_outbox_drops_the_oldest_sample() {
        quiet(|| {
            let mut link = link_holding(full_queue(OUTBOX_MAX, usize::MAX, sample(-1)));
            assert_eq!(link.outbox_len(), OUTBOX_MAX);
            link.send(sample(1000))
                .expect("a sample is always allowed in");
            assert_eq!(link.outbox_len(), OUTBOX_MAX);
            assert_eq!(
                link.outbox.front(),
                Some(&sample(1)),
                "the oldest sample gave its place"
            );
            assert_eq!(
                link.outbox.back(),
                Some(&sample(1000)),
                "the newest is kept"
            );
        });
    }

    /// A size for an app that has stopped reading: it takes a sample's place rather than being lost, because it is not a
    /// sample and there is nothing of its own to replace.
    #[test]
    fn a_state_message_takes_a_samples_place() {
        quiet(|| {
            let mut link = link_holding(full_queue(OUTBOX_MAX, usize::MAX, sample(-1)));
            let size = ToApp::Configure {
                width: 1024,
                height: 768,
                scale: 1,
                focused: true,
            };
            link.send(size).expect("room was made");
            assert_eq!(link.outbox_len(), OUTBOX_MAX);
            assert_eq!(link.outbox.back(), Some(&size));
            assert_eq!(link.outbox.iter().filter(|m| **m == size).count(), 1);
            assert_eq!(
                link.outbox.front(),
                Some(&sample(1)),
                "at the price of the oldest sample"
            );
        });
    }

    /// Two sizes in the queue are one size too many, and so are two visibility states: the newer one stands where the older
    /// one stood, and the queue does not grow. Two inputs are never one of those — a press and its release are both `input`,
    /// and losing one leaves a key down.
    #[test]
    fn a_newer_state_message_replaces_the_older_one() {
        quiet(|| {
            let first = ToApp::Configure {
                width: 800,
                height: 600,
                scale: 1,
                focused: false,
            };
            let last = ToApp::Configure {
                width: 1024,
                height: 768,
                scale: 1,
                focused: true,
            };
            let mut link = link_holding(full_queue(OUTBOX_MAX, 5, first));
            link.send(last).expect("room was made");
            assert_eq!(link.outbox_len(), OUTBOX_MAX, "the queue did not grow");
            assert_eq!(link.outbox[5], last, "in the place of the older size");
            assert_eq!(link.outbox.iter().filter(|m| **m == first).count(), 0);
            assert_eq!(
                link.outbox.iter().filter(|m| **m == last).count(),
                1,
                "and once, not twice"
            );

            // The two visibility states are one variable too.
            let mut link = link_holding(full_queue(OUTBOX_MAX, 2, ToApp::Background));
            link.send(ToApp::Foreground).expect("room was made");
            assert_eq!(link.outbox[2], ToApp::Foreground);
            assert_eq!(link.outbox_len(), OUTBOX_MAX);

            // And a press and its release both stay, in the order they happened.
            let mut link = link_holding([sample(1), sample(0)]);
            link.send(sample(2)).expect("room was made");
            assert_eq!(
                link.outbox.iter().copied().collect::<Vec<_>>(),
                vec![sample(1), sample(0), sample(2)],
                "a queue that was not full just grew, in order"
            );
        });
    }

    /// `close` is the one message the queue will not make room for by losing something: a full outbox refuses it, loudly, so
    /// the caller learns the app did not hear. With a slot free it goes, and the newest thing in the queue is the order to end.
    #[test]
    fn a_full_outbox_refuses_a_close() {
        quiet(|| {
            let mut link = link_holding(full_queue(OUTBOX_MAX, usize::MAX, sample(-1)));
            let e = link
                .send(ToApp::Close)
                .expect_err("no room, and no dropping either");
            assert_eq!(e.raw_os_error(), Some(libc::EOVERFLOW), "{e}");
            assert_eq!(link.outbox_len(), OUTBOX_MAX, "and the queue is as it was");

            let mut link = link_holding(full_queue(OUTBOX_MAX - 1, usize::MAX, sample(-1)));
            link.send(ToApp::Close).expect("one slot free");
            assert_eq!(link.outbox_len(), OUTBOX_MAX);
            assert_eq!(link.outbox.back(), Some(&ToApp::Close));
        });
    }

    /// The policy's last arm: with nothing droppable in the queue, a newcomer that says nothing new is the one that goes. The
    /// public API cannot build a queue like this, because every state message replaces its own kind, but a smaller
    /// [`OUTBOX_MAX`] could, so the arm stays — and this says it does nothing rather than losing a `close`.
    #[test]
    fn a_queue_with_nothing_to_drop_lets_the_newcomer_go() {
        quiet(|| {
            let held: Vec<ToApp> = std::iter::repeat_n(ToApp::Close, OUTBOX_MAX).collect();
            let mut link = link_holding(held);
            let size = ToApp::Configure {
                width: 640,
                height: 480,
                scale: 1,
                focused: false,
            };
            link.send(size).expect("nothing is refused here");
            assert_eq!(link.outbox_len(), OUTBOX_MAX);
            assert_eq!(
                link.outbox.iter().filter(|m| **m == size).count(),
                0,
                "the newcomer was dropped, not queued behind a row of closes"
            );
            assert_eq!(
                link.outbox.iter().filter(|m| **m == ToApp::Close).count(),
                OUTBOX_MAX,
                "and none of the closes was lost for it"
            );
        });
    }

    /// What waits in the queue goes out in the order it was queued, and the queue is empty afterwards.
    #[test]
    fn flush_sends_the_queue_in_order() {
        quiet(|| {
            let (mut shell, app) = Link::pair().expect("socketpair");
            let held = [sample(7), ToApp::Foreground, ToApp::Close];
            shell.outbox = held.into_iter().collect();
            assert_eq!(shell.flush().expect("the socket takes three"), 3);
            assert_eq!(shell.outbox_len(), 0);
            let mut app = Link::from_fd(app);
            for m in held {
                assert_eq!(app.recv_from_shell().expect("in order"), Some(m));
            }
        });
    }

    /// The number the app's end takes: the next free one at or above [`APP_FD`], never the hold pipe's 3
    /// (`lucina_app::hold::FD`), and the moved end is the same socket. The exact number is not asserted, because another test
    /// in this process may hold 4; the rule the child depends on is the floor and the working descriptor.
    #[test]
    fn the_app_end_takes_the_next_free_slot() {
        quiet(|| {
            let (mut shell, app) = Link::pair().expect("socketpair");
            let placed = Link::for_child(app).expect("the slot is asked for");
            assert!(
                placed.as_raw_fd() >= APP_FD,
                "{} is below the named slot",
                placed.as_raw_fd()
            );
            assert_ne!(
                placed.as_raw_fd(),
                lucina_app::hold::FD,
                "not the hold pipe's number"
            );
            let mut app = Link::from_fd(placed);
            let m = ToApp::Close;
            shell.send(m).expect("sent");
            assert_eq!(app.recv_from_shell().expect("received"), Some(m));
            // Close-on-exec still: the spawn path clears the flag for the one descriptor the child keeps.
            assert_eq!(sys::probe(app.fd()), (true, true));
        });
    }

    /// The whole module as the shell's event loop will use it: one `Link` in one thread, every message the shell may say sent
    /// to it, while a second thread reads the app's end and never lets the socket fill. Nothing waits, the queue stays empty
    /// because the peer takes what arrives, every message is what was sent, and the process holds the same number of
    /// descriptors afterwards as before: a conversation costs no descriptor, in either thread.
    #[test]
    fn a_whole_conversation_leaves_no_descriptor_behind() {
        quiet(|| {
            let before = open_fds();
            let cases = app_cases();
            let (mut shell, app) = Link::pair().expect("socketpair");
            let mut app = Link::from_fd(app);
            // The reading end lives elsewhere, so a write is never a wait: the shell's end stays non-blocking and the queue
            // never fills, which is the shape slice W will run in.
            let reader = std::thread::spawn(move || {
                let mut got = Vec::new();
                loop {
                    match app.recv_from_shell() {
                        Ok(Some(m)) => got.push(m),
                        // An interrupted read took nothing away, so the app asks again. On this end, which blocks, that is the
                        // only way `None` arrives.
                        Ok(None) => {}
                        // The shell's end is gone: an app learns it has no shell as one error, not as a hang.
                        Err(ref e) if e.kind() == ErrorKind::UnexpectedEof => break got,
                        Err(e) => panic!("the app end: {e}"),
                    }
                }
            });
            for m in &cases {
                shell.send(*m).expect("the socket takes it");
                assert_eq!(
                    shell.outbox_len(),
                    0,
                    "{m:?} queued although a thread reads the other end"
                );
            }
            assert_eq!(
                shell.flush().expect("nothing to send"),
                0,
                "the queue was empty"
            );
            drop(shell);
            let got = reader
                .join()
                .expect("the reader reached the end of the socket");
            assert_eq!(got, cases, "every message, whole, in the order they went");
            assert_eq!(open_fds(), before, "and no descriptor left behind");
        });
    }
}
