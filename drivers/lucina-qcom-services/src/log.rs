// SPDX-License-Identifier: BSD-3-Clause

use alloc::collections::VecDeque;
use alloc::string::String;

const LINES: usize = 64;
const LINE_CHARS: usize = 160;

/// The lines the C daemons print, for the kernel to read: bounded, because file names in them come from the
/// modem. When it is full the oldest line goes.
#[derive(Debug, Default)]
pub struct Log {
    lines: VecDeque<String>,
}

impl Log {
    pub fn new() -> Log {
        Log::default()
    }

    pub fn push(&mut self, line: &str) {
        if self.lines.len() >= LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(
            line.chars()
                .take(LINE_CHARS)
                .map(|c| if c.is_control() { '?' } else { c })
                .collect(),
        );
    }

    pub fn pop(&mut self) -> Option<String> {
        self.lines.pop_front()
    }
}
