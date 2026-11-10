//! Decoding on the laptop's video block through `/dev/lucina-venus` (the kernel's `qcom_venus`, `docs/research/juno-venus.md`): access
//! units go into the kernel's input buffers through their mappings, pictures come back as NV12 in its picture buffers, and each is
//! given back once copied out. Without the device, with `LUCINA_VIDEO_VENUS=0`, or when the block refuses the stream, the player uses
//! the CPU decoders as before.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration, Instant};

use lucina_video::Codec;

const DEVICE: &str = "/dev/lucina-venus";
const MMAP_STEP: u64 = 1 << 40;
const PICTURE_SLOT: u64 = 33;
/// How long a picture may take before the block is taken as stuck: far above a frame at any rate this plays.
const STUCK: Duration = Duration::from_secs(2);

struct Map {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: the mapping is the decoder's own and lives as long as it; only the decoder's thread touches it.
unsafe impl Send for Map {}

impl Map {
    fn new(file: &File, slot: u64, len: usize) -> io::Result<Map> {
        // SAFETY: a fresh shared mapping of the device at the offset the kernel names for this buffer; checked for failure below.
        let ptr = unsafe {
            libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, file.as_raw_fd(), (slot * MMAP_STEP) as libc::off_t)
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Map { ptr: ptr.cast(), len })
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: `len` bytes mapped at `ptr` for the life of `self`.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as `bytes`, and `&mut self` makes it the only view.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for Map {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly what `new` mapped.
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}

/// Where the planes of the block's pictures are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub uv_offset: usize,
    pub bytes: usize,
    pub crop: [usize; 4],
}

/// One picture: which buffer, its time, where it lies.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub index: usize,
    pub pts_us: i64,
    pub offset: usize,
    pub len: usize,
}

/// What the kernel said, in its order: a size change comes between the last picture of the old size and the first of the new one.
enum Ready {
    Frame(Frame),
    Size(Layout, usize),
}

pub struct Venus {
    file: File,
    inputs: Vec<Map>,
    free_inputs: VecDeque<usize>,
    pictures: Vec<Map>,
    pub layout: Option<Layout>,
    ready: VecDeque<Ready>,
    pub ended: bool,
    seeked: bool,
    pending: String,
}

fn codec_name(c: Codec) -> Option<&'static str> {
    Some(match c {
        Codec::H264 => "h264",
        Codec::Hevc => "hevc",
        Codec::Vp9 => "vp9",
        Codec::Vp8 => "vp8",
        Codec::Mpeg2 => "mpeg2",
        _ => return None,
    })
}

fn bad(what: impl Into<String>) -> io::Error {
    io::Error::other(what.into())
}

impl Venus {
    /// `None` when nothing here can be tried: no device, turned off, or a codec the block does not have.
    pub fn open(codec: Codec, width: u32, height: u32) -> Option<io::Result<Venus>> {
        if std::env::var_os("LUCINA_VIDEO_VENUS").is_some_and(|v| v == "0") {
            return None;
        }
        let name = codec_name(codec)?;
        let file = OpenOptions::new().read(true).write(true).custom_flags(libc::O_NONBLOCK).open(DEVICE).ok()?;
        Some(Venus::start(file, name, width, height))
    }

    fn start(mut file: File, name: &str, width: u32, height: u32) -> io::Result<Venus> {
        file.write_all(format!("open {name} {width} {height}").as_bytes())?;
        let mut v = Venus {
            file,
            inputs: Vec::new(),
            free_inputs: VecDeque::new(),
            pictures: Vec::new(),
            layout: None,
            ready: VecDeque::new(),
            ended: false,
            seeked: false,
            pending: String::new(),
        };
        let start = Instant::now();
        while v.inputs.is_empty() {
            v.wait_events()?;
            if start.elapsed() > STUCK {
                return Err(bad("the video block did not say its input buffers"));
            }
        }
        Ok(v)
    }

    fn command(&mut self, line: &str) -> io::Result<()> {
        self.file.write_all(line.as_bytes())
    }

    /// Reads what the kernel has, without waiting (the file is non-blocking). Whether anything came.
    fn read_events(&mut self) -> io::Result<bool> {
        let mut buf = [0u8; 4096];
        let n = match self.file.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => 0,
            Err(e) => return Err(e),
        };
        self.pending.push_str(&String::from_utf8_lossy(&buf[..n]));
        while let Some(at) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=at).collect();
            self.event(line.trim())?;
        }
        Ok(n != 0)
    }

    /// As [`Self::read_events`]; when nothing came, one read that waits in the kernel. With patch 1161 the kernel sleeps until
    /// the video block interrupts (at most 50 ms); an older kernel looks every millisecond itself. Either way this thread no
    /// longer wakes every millisecond to look again. If the file's flags cannot be changed, a millisecond's pause, as before.
    fn wait_events(&mut self) -> io::Result<()> {
        if self.read_events()? {
            return Ok(());
        }
        if self.set_blocking(true) {
            let got = self.read_events();
            self.set_blocking(false);
            got?;
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }

    /// Clears or sets the file's `O_NONBLOCK`. Whether it worked.
    fn set_blocking(&self, blocking: bool) -> bool {
        let fd = self.file.as_raw_fd();
        // SAFETY: fcntl on a file descriptor this struct owns, with the flag commands only.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return false;
        }
        let flags = if blocking { flags & !libc::O_NONBLOCK } else { flags | libc::O_NONBLOCK };
        // SAFETY: as above.
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags) == 0 }
    }

    fn event(&mut self, line: &str) -> io::Result<()> {
        let w: Vec<&str> = line.split_ascii_whitespace().collect();
        let num = |i: usize| w.get(i).and_then(|v| v.parse::<i64>().ok()).ok_or_else(|| bad(format!("a line the kernel should not send: {line}")));
        match w.first().copied() {
            Some("inputs") => {
                let (count, bytes) = (num(1)? as usize, num(2)? as usize);
                for i in 0..count {
                    self.inputs.push(Map::new(&self.file, i as u64 + 1, bytes)?);
                    self.free_inputs.push_back(i);
                }
            }
            Some("in") => self.free_inputs.push_back(num(1)? as usize),
            Some("size") => {
                let crop = [num(7)? as usize, num(8)? as usize, num(9)? as usize, num(10)? as usize];
                let l = Layout {
                    width: num(1)? as usize,
                    height: num(2)? as usize,
                    stride: num(3)? as usize,
                    uv_offset: num(4)? as usize,
                    bytes: num(5)? as usize,
                    crop,
                };
                let count = num(6)? as usize;
                if self.pictures.is_empty() {
                    self.resize(l, count)?;
                } else {
                    // The stream changed size: the pictures of the old size still queued are shown first.
                    self.ready.push_back(Ready::Size(l, count));
                }
            }
            Some("pic") => {
                let f = Frame { index: num(1)? as usize, pts_us: num(2)?, offset: num(3)? as usize, len: num(4)? as usize };
                self.ready.push_back(Ready::Frame(f));
            }
            Some("seeked") => self.seeked = true,
            Some("eos") => self.ended = true,
            Some("error") => return Err(bad(format!("the video block: {line}"))),
            _ => {}
        }
        Ok(())
    }

    /// One access unit in, waiting for an input buffer to be free.
    pub fn send(&mut self, data: &[u8], pts_us: i64) -> io::Result<()> {
        let start = Instant::now();
        let i = loop {
            if let Some(i) = self.free_inputs.pop_front() {
                break i;
            }
            self.wait_events()?;
            if start.elapsed() > STUCK {
                return Err(bad("the video block keeps every input buffer"));
            }
        };
        let room = self.inputs[i].len;
        if data.is_empty() || data.len() > room {
            self.free_inputs.push_front(i);
            return Err(bad(format!("an access unit of {} bytes for buffers of {room}", data.len())));
        }
        self.inputs[i].bytes_mut()[..data.len()].copy_from_slice(data);
        self.command(&format!("in {i} {} {}", data.len(), pts_us.max(0)))
    }

    /// The picture buffers of a new size, the old ones unmapped: the kernel gave their numbers to new buffers.
    fn resize(&mut self, l: Layout, count: usize) -> io::Result<()> {
        self.pictures.clear();
        for i in 0..count {
            self.pictures.push(Map::new(&self.file, PICTURE_SLOT + i as u64, l.bytes)?);
        }
        self.layout = Some(l);
        Ok(())
    }

    /// The next picture in the kernel's order, size changes applied on the way.
    fn next_ready(&mut self) -> io::Result<Option<Frame>> {
        while let Some(r) = self.ready.pop_front() {
            match r {
                Ready::Size(l, count) => self.resize(l, count)?,
                Ready::Frame(f) => {
                    if f.index >= self.pictures.len() || f.offset.checked_add(f.len).is_none_or(|end| end > self.pictures[f.index].len) {
                        return Err(bad(format!("a picture outside its buffer: {} at {}+{}", f.index, f.offset, f.len)));
                    }
                    return Ok(Some(f));
                }
            }
        }
        Ok(None)
    }

    /// The next picture decoded so far, without waiting.
    pub fn take(&mut self) -> io::Result<Option<Frame>> {
        if self.ready.is_empty() && !self.ended {
            self.read_events()?;
        }
        self.next_ready()
    }

    /// Waits for the next picture or the end, at most `limit`.
    pub fn take_waiting(&mut self, limit: Duration) -> io::Result<Option<Frame>> {
        let start = Instant::now();
        while self.ready.is_empty() && !self.ended && start.elapsed() < limit {
            self.wait_events()?;
        }
        self.next_ready()
    }

    /// A new position in the same session (Linux's STREAMOFF and STREAMON on the bitstream queue): what was queued or decoded and
    /// not taken is dropped, every input buffer is free again, and the next input must start at a key frame.
    pub fn seek(&mut self) -> io::Result<()> {
        self.seeked = false;
        self.command("seek")?;
        let start = Instant::now();
        while !self.seeked {
            self.wait_events()?;
            if start.elapsed() > STUCK {
                return Err(bad("the video block did not finish the seek"));
            }
        }
        // Lines read before "seeked" are of the old position; a size change among them still applies.
        let sizes: Vec<(Layout, usize)> = self.ready.drain(..).filter_map(|r| if let Ready::Size(l, c) = r { Some((l, c)) } else { None }).collect();
        if let Some((l, c)) = sizes.last() {
            self.resize(*l, *c)?;
        }
        self.free_inputs = (0..self.inputs.len()).collect();
        self.ended = false;
        Ok(())
    }

    pub fn end(&mut self) -> io::Result<()> {
        self.command("eos")
    }

    /// The picture's bytes, until [`Self::give_back`].
    pub fn bytes(&self, f: &Frame) -> &[u8] {
        &self.pictures[f.index].bytes()[f.offset..f.offset + f.len]
    }

    pub fn give_back(&mut self, f: &Frame) -> io::Result<()> {
        self.command(&format!("out {}", f.index))
    }
}

impl Drop for Venus {
    fn drop(&mut self) {
        // The mappings go first, so that the kernel can free the buffers when the session ends (fields would drop only after this,
        // after `close`); then the session ends, and closing the file after it.
        self.inputs.clear();
        self.pictures.clear();
        let _ = self.command("close");
    }
}

/// NV12 to the planar form the player shows: the luma lines as they are, the chroma pairs split, all of it cropped to `w` x `h`.
pub fn nv12_to_i420(src: &[u8], l: &Layout, w: usize, h: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let (left, top) = (l.crop[0], l.crop[1]);
    let mut y = Vec::with_capacity(w * h);
    for row in 0..h {
        let at = (top + row) * l.stride + left;
        y.extend_from_slice(&src[at..at + w]);
    }
    let mut u = Vec::with_capacity(cw * ch);
    let mut v = Vec::with_capacity(cw * ch);
    for row in 0..ch {
        let at = l.uv_offset + (top / 2 + row) * l.stride + left / 2 * 2;
        for pair in src[at..at + cw * 2].chunks_exact(2) {
            u.push(pair[0]);
            v.push(pair[1]);
        }
    }
    (y, u, v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv12_splits_and_crops() {
        // 4 x 2 picture, stride 8, UV plane at 16.
        let mut src = vec![0u8; 24];
        for (i, b) in src[..16].iter_mut().enumerate() {
            *b = i as u8;
        }
        src[16..24].copy_from_slice(&[100, 200, 101, 201, 0, 0, 0, 0]);
        let l = Layout { width: 4, height: 2, stride: 8, uv_offset: 16, bytes: 24, crop: [0, 0, 4, 2] };
        let (y, u, v) = nv12_to_i420(&src, &l, 4, 2);
        assert_eq!(y, [0, 1, 2, 3, 8, 9, 10, 11]);
        assert_eq!(u, [100, 101]);
        assert_eq!(v, [200, 201]);
    }
}
