# sunset 0.6.0 for LucinaOS

Verdict: **adapt**. The crates.io release 0.6.0 (0BSD, sha256 of the .crate `3b52312b…bad78f`) was committed unmodified
first, so `git log -p third_party/sunset` shows exactly what LucinaOS changed. `lucina-sshd` (crates/lucina-remote) is
its only user. All changes are marked `LucinaOS` in the source.

| Change | Why |
|---|---|
| `Runner::channel_exit_status`: exit-status, EOF, close | A sunset server could not end a session: `ssh host cmd` got no status and exited 255 |
| No EOF sent in reply to the client's EOF, on a server | EOF ends one direction: `ssh host cmd < file` lost the program's output after stdin ended |
| `ServPtyRequest::term_size` | The shell's `TERM` and window size |
| With the `larger` feature: 2 MiB receive window, 32 KiB packets | At 1000 bytes per round trip a 20 MB kernel upload takes minutes |

Upstream contributions are drafts only, without the owner's OK.
