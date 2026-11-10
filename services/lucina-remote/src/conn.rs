//! One SSH connection: sunset's `Runner` driven by `poll(2)` over the socket and the sessions' programs.
//!
//! Authentication is public key only, for user `root`, against the keys of `authorized_keys`.

use std::{
    collections::HashMap,
    io::{self, ErrorKind, Read, Write},
    net::TcpStream,
    os::fd::{AsFd, OwnedFd},
    time::Duration,
};

use log::{info, warn};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use sunset::{ChanData, ChanHandle, ChanNum, Event, Runner, ServEvent, SignKey};

use crate::{
    keys::{AuthorizedKey, fingerprint},
    process::{self, Process, PtyRequest, ReadResult},
};

pub struct Config {
    pub host_key: SignKey,
    pub authorized: Vec<AuthorizedKey>,
    pub shell: String,
}

struct Session {
    chan: ChanHandle,
    pty: Option<PtyRequest>,
    env: Vec<(String, String)>,
    proc: Option<Process>,
    /// Channel data waiting for the program's stdin.
    pending_in: Vec<u8>,
    out_eof: bool,
    err_eof: bool,
    exit_sent: bool,
}

impl Session {
    fn new(chan: ChanHandle) -> Self {
        Session {
            chan,
            pty: None,
            env: Vec::new(),
            proc: None,
            pending_in: Vec::new(),
            out_eof: false,
            err_eof: false,
            exit_sent: false,
        }
    }
}

/// Environment variables a client may set (as OpenSSH's default `AcceptEnv`).
fn accept_env(name: &str) -> bool {
    name == "TERM" || name == "LANG" || name.starts_with("LC_")
}

pub fn serve(mut sock: TcpStream, cfg: &Config) -> io::Result<()> {
    let peer = sock.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    sock.set_nodelay(true)?;
    sock.set_nonblocking(true)?;
    let mut runner = Runner::new_server_owned();
    let mut sessions: HashMap<ChanNum, Session> = HashMap::new();
    let mut rx = vec![0u8; 16384];
    let (mut rx_start, mut rx_end) = (0usize, 0usize);
    let mut buf = vec![0u8; 32768];
    // The command of a limited key this client has offered. It never goes away, in whichever order keys are offered: a client
    // that holds a full key has no reason to offer a limited one first.
    let mut forced: Option<String> = None;
    let mut net_eof = false;
    let err = |e: sunset::Error| io::Error::other(format!("ssh: {e}"));

    loop {
        // Whether this pass moved anything; only a pass that did not may block in poll(), since sunset's own
        // readiness (input accepted again once a channel was read) has no file descriptor.
        let mut busy = false;

        // 1. Bytes from the client into sunset.
        if rx_start == rx_end && !net_eof && runner.is_input_ready() {
            match sock.read(&mut rx) {
                Ok(0) => {
                    net_eof = true;
                    busy = true;
                    runner.close_input();
                }
                Ok(n) => {
                    (rx_start, rx_end) = (0, n);
                    busy = true;
                }
                Err(e)
                    if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {}
                Err(_) => {
                    net_eof = true;
                    runner.close_input();
                }
            }
        }
        while rx_start < rx_end && runner.is_input_ready() {
            let n = runner.input(&rx[rx_start..rx_end]).map_err(err)?;
            if n == 0 {
                break;
            }
            rx_start += n;
            busy = true;
        }

        // 2. Protocol events.
        loop {
            let ev = runner.progress().map_err(err)?;
            busy |= !matches!(ev, Event::None);
            match ev {
                Event::Serv(ev) => match ev {
                    ServEvent::Hostkeys(h) => h.hostkeys(&[&cfg.host_key]).map_err(err)?,
                    ServEvent::FirstAuth(mut a) => {
                        a.set_auth_methods(false, true).map_err(err)?;
                        a.reject().map_err(err)?;
                    }
                    ServEvent::PasswordAuth(a) => a.reject().map_err(err)?,
                    ServEvent::PubkeyAuth(a) => {
                        let user_ok = a.username().is_ok_and(|u| u == "root");
                        let key = match a.pubkey() {
                            Ok(sunset::PubKey::Ed25519(k)) => Some(k.key.0),
                            _ => None,
                        };
                        match key
                            .and_then(|k| cfg.authorized.iter().find(|ak| ak.key == k).map(|_| k))
                        {
                            Some(k) if user_ok => {
                                if let Some(ak) = cfg.authorized.iter().find(|ak| ak.key == k)
                                    && ak.command.is_some()
                                {
                                    forced = ak.command.clone();
                                }
                                info!(
                                    "{peer}: key {} ({}) accepted",
                                    fingerprint(&k),
                                    comment(cfg, &k)
                                );
                                a.allow().map_err(err)?
                            }
                            _ => a.reject().map_err(err)?,
                        }
                    }
                    ServEvent::Authenticated => info!("{peer}: authenticated as root"),
                    ServEvent::OpenSession(s) => {
                        let chan = s.accept().map_err(err)?;
                        sessions.insert(chan.num(), Session::new(chan));
                    }
                    ServEvent::SessionPty(p) => {
                        let req = p.term_size().ok().map(|(term, cols, rows)| PtyRequest {
                            term: term.to_string(),
                            cols: cols.min(u16::MAX as u32) as u16,
                            rows: rows.min(u16::MAX as u32) as u16,
                        });
                        match (sessions.get_mut(&p.channel()), req) {
                            (Some(s), Some(req)) if s.proc.is_none() && forced.is_none() => {
                                s.pty = Some(req);
                                p.succeed().map_err(err)?
                            }
                            _ => p.fail().map_err(err)?,
                        }
                    }
                    ServEvent::SessionEnv(e) => {
                        let kv = e
                            .name()
                            .ok()
                            .zip(e.value().ok())
                            .map(|(k, v)| (k.to_string(), v.to_string()));
                        match (sessions.get_mut(&e.channel()), kv) {
                            (Some(s), Some((k, v))) if accept_env(&k) && forced.is_none() => {
                                s.env.push((k, v));
                                e.succeed().map_err(err)?
                            }
                            _ => e.fail().map_err(err)?,
                        }
                    }
                    ServEvent::SessionShell(r) => {
                        let ok = start(&mut sessions, r.channel(), forced.as_deref(), cfg, &peer);
                        if ok { r.succeed() } else { r.fail() }.map_err(err)?
                    }
                    ServEvent::SessionExec(r) => {
                        let command = r.command().map(str::to_string);
                        let ok = match command {
                            Ok(c) => {
                                let c = forced.as_deref().unwrap_or(&c);
                                start(&mut sessions, r.channel(), Some(c), cfg, &peer)
                            }
                            Err(_) => false,
                        };
                        if ok { r.succeed() } else { r.fail() }.map_err(err)?
                    }
                    ServEvent::SessionSubsystem(r) => r.fail().map_err(err)?,
                    ServEvent::Defunct => {
                        info!("{peer}: connection closed");
                        for s in sessions.values_mut() {
                            if let Some(p) = s.proc.as_mut() {
                                p.kill();
                            }
                        }
                        return Ok(());
                    }
                    ServEvent::PollAgain => {}
                },
                Event::Progressed => {}
                Event::None => break,
                Event::Cli(_) => unreachable!("server runner"),
            }
        }

        // 3. Channel data to the programs, and programs' output to the channels.
        while let Some((num, dt, _)) = runner.read_channel_ready() {
            let Some(s) = sessions.get_mut(&num) else {
                break;
            };
            if dt != ChanData::Normal
                || s.proc.is_none()
                || s.proc.as_ref().is_some_and(|p| p.stdin.is_none())
            {
                runner.discard_read_channel(&s.chan).map_err(err)?;
                busy = true;
                continue;
            }
            if !s.pending_in.is_empty() {
                break;
            }
            let n = runner
                .read_channel(&s.chan, ChanData::Normal, &mut buf)
                .map_err(err)?;
            s.pending_in.extend_from_slice(&buf[..n]);
            busy |= n > 0;
            if n == 0 {
                break;
            }
        }
        let mut finished = Vec::new();
        for (num, s) in sessions.iter_mut() {
            let Some(p) = s.proc.as_mut() else { continue };
            if let Some(fd) = &p.stdin {
                if !s.pending_in.is_empty() {
                    match process::write_nb(fd, &s.pending_in) {
                        Ok(n) => {
                            busy |= n > 0;
                            s.pending_in.drain(..n);
                        }
                        Err(_) => {
                            s.pending_in.clear();
                            p.stdin = None;
                        }
                    }
                }
                // The client's EOF closes the program's stdin (not with a pty: the shell keeps the master).
                if s.pending_in.is_empty() && runner.is_channel_eof(&s.chan) && s.pty.is_none() {
                    p.stdin = None;
                    busy = true;
                }
            }
            busy |= pump(
                &mut runner,
                &s.chan,
                ChanData::Normal,
                p.stdout.as_ref(),
                &mut s.out_eof,
                &mut buf,
            )
            .map_err(err)?;
            if p.stderr.is_some() {
                busy |= pump(
                    &mut runner,
                    &s.chan,
                    ChanData::Stderr,
                    p.stderr.as_ref(),
                    &mut s.err_eof,
                    &mut buf,
                )
                .map_err(err)?;
            } else {
                s.err_eof = true;
            }
            if s.out_eof
                && s.err_eof
                && !s.exit_sent
                && let Some(code) = p.try_exit()
                && matches!(runner.write_channel_ready(&s.chan, ChanData::Normal), Ok(Some(n)) if n > 0)
            {
                runner.channel_exit_status(&s.chan, code).map_err(err)?;
                s.exit_sent = true;
                finished.push(*num);
            }
            if runner.is_channel_closed(&s.chan) && !s.exit_sent {
                // The client closed the channel first.
                p.kill();
                s.exit_sent = true;
                finished.push(*num);
            }
        }
        busy |= !finished.is_empty();
        for num in finished {
            if let Some(s) = sessions.remove(&num) {
                runner.channel_done(s.chan).map_err(err)?;
            }
        }

        // 4. sunset's output to the client.
        loop {
            let out = runner.output_buf();
            if out.is_empty() {
                break;
            }
            match sock.write(out) {
                Ok(n) => {
                    busy |= n > 0;
                    runner.consume_output(n);
                }
                Err(e)
                    if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted =>
                {
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        // 5. Wait for something to do.
        if !busy {
            wait(&sock, &runner, &sessions, rx_start < rx_end, net_eof)?;
        }
    }
}

fn comment<'a>(cfg: &'a Config, key: &[u8; 32]) -> &'a str {
    cfg.authorized
        .iter()
        .find(|a| &a.key == key)
        .map_or("", |a| a.comment.as_str())
}

fn start(
    sessions: &mut HashMap<ChanNum, Session>,
    num: ChanNum,
    command: Option<&str>,
    cfg: &Config,
    peer: &str,
) -> bool {
    let Some(s) = sessions.get_mut(&num) else {
        return false;
    };
    if s.proc.is_some() {
        return false;
    }
    match process::spawn(&cfg.shell, command, s.pty.as_ref(), &s.env) {
        Ok(p) => {
            info!(
                "{peer}: {} started{}",
                command.map_or("shell".to_string(), |c| format!("{c:?}")),
                if s.pty.is_some() { " on a pty" } else { "" }
            );
            s.proc = Some(p);
            true
        }
        Err(e) => {
            warn!(
                "{peer}: cannot start {}: {e}",
                command.unwrap_or(&cfg.shell)
            );
            false
        }
    }
}

/// Moves what the program wrote on `fd` to the channel, as much as the channel window allows; returns whether
/// anything moved or ended.
fn pump(
    runner: &mut Runner<'static, sunset::Server>,
    chan: &ChanHandle,
    dt: ChanData,
    fd: Option<&OwnedFd>,
    eof: &mut bool,
    buf: &mut [u8],
) -> sunset::Result<bool> {
    let Some(fd) = fd else {
        *eof = true;
        return Ok(false);
    };
    let mut moved = false;
    while !*eof {
        let space = match runner.write_channel_ready(chan, dt)? {
            Some(n) if n > 0 => n.min(buf.len()),
            Some(_) => return Ok(moved),
            None => {
                *eof = true;
                return Ok(true);
            }
        };
        match process::read_nb(fd, &mut buf[..space]) {
            ReadResult::Data(n) => {
                let mut off = 0;
                while off < n {
                    let w = runner.write_channel(chan, dt, &buf[off..n])?;
                    if w == 0 {
                        // Cannot happen: write_channel_ready reported the space.
                        return sunset::Error::bug();
                    }
                    off += w;
                }
                moved = true;
            }
            ReadResult::WouldBlock => return Ok(moved),
            ReadResult::Eof => {
                *eof = true;
                moved = true;
            }
        }
    }
    Ok(moved)
}

fn wait(
    sock: &TcpStream,
    runner: &Runner<'static, sunset::Server>,
    sessions: &HashMap<ChanNum, Session>,
    rx_pending: bool,
    net_eof: bool,
) -> io::Result<()> {
    let mut sock_flags = PollFlags::empty();
    if !net_eof && !rx_pending && runner.is_input_ready() {
        sock_flags |= PollFlags::IN;
    }
    if runner.is_output_pending() {
        sock_flags |= PollFlags::OUT;
    }
    let mut fds = vec![PollFd::from_borrowed_fd(sock.as_fd(), sock_flags)];
    let mut need_exit_poll = false;
    for s in sessions.values() {
        let Some(p) = &s.proc else { continue };
        let window =
            matches!(runner.write_channel_ready(&s.chan, ChanData::Normal), Ok(Some(n)) if n > 0);
        if let (Some(fd), false, true) = (&p.stdout, s.out_eof, window) {
            fds.push(PollFd::from_borrowed_fd(fd.as_fd(), PollFlags::IN));
        }
        if let (Some(fd), false, true) = (&p.stderr, s.err_eof, window) {
            fds.push(PollFd::from_borrowed_fd(fd.as_fd(), PollFlags::IN));
        }
        if let (Some(fd), false) = (&p.stdin, s.pending_in.is_empty()) {
            fds.push(PollFd::from_borrowed_fd(fd.as_fd(), PollFlags::OUT));
        }
        // There is no fd for the program's exit; look again soon once its output has ended.
        need_exit_poll |= s.out_eof && s.err_eof && !s.exit_sent;
    }
    // Wake up regularly anyway: sunset state that has no fd (a channel that became readable while its program's
    // stdin was full) is rechecked on the next pass.
    let timeout = if need_exit_poll {
        Duration::from_millis(20)
    } else {
        Duration::from_millis(500)
    };
    let ts = Timespec {
        tv_sec: timeout.as_secs() as _,
        tv_nsec: timeout.subsec_nanos() as _,
    };
    match poll(&mut fds, Some(&ts)) {
        Ok(_) | Err(rustix::io::Errno::INTR) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
