//! The client's side of one voice socket: which turn is open, what each message from the server means for it, and what goes
//! up. No I/O and no clock of its own: messages come in, [`Action`]s come out, and `now` is passed in. The phases and the table
//! of waiting notes are [`crate::rules`] (proved); this file keeps the strings and the timings next to them.
use std::time::Instant;

use crate::msg::{self, Purpose, Server, Start};
use crate::rules::{self, Notes};

/// What may wait for the server's `listening`: 3 s of 16 kHz mono.
pub const HOLD_CAP: usize = 96_000;
/// The protocol's cap on one binary frame.
pub const MAX_FRAME: usize = 8192;

/// What the screen is told. Texts are the owner's and Lucina's words: never logged.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The socket is up (the voice keys work only while it is).
    Connected,
    /// The socket is down, with a short reason for the screen (`signed out`, `no connection` ...); it reconnects by itself.
    Disconnected(String),
    /// The server is listening: talk.
    Listening,
    /// The server heard enough (a pause, or the cap): the turn goes on without more audio.
    Endpoint,
    Transcript(String),
    /// A fragment of Lucina's command reply as she writes it.
    Delta(String),
    /// The whole reply; with `replaces` it stands instead of the deltas.
    Answer { text: String, replaces: bool },
    /// A command's reply, settled.
    Reply(String),
    /// The command turn is over.
    Done,
    /// A voice note is a chat message now; the timeline stream shows the real item.
    NoteStored { share_id: String, user_item_id: Option<String> },
    NoteReply { share_id: String, text: String, item_id: Option<String> },
    NoteDone { share_id: String },
    /// More than four notes waiting: the server stopped watching this one (it is in the chat all the same).
    NoteDropped { share_id: String },
    NoteFailed { share_id: String, code: String, message: String },
    /// The open turn failed (server error, or the socket closed).
    Failed { code: String, message: String },
    /// A server from before section 10: no voice notes.
    OldServer,
    /// A timing, never any content: `what` names the span from the owner's end of speech.
    Timing { what: &'static str, ms: u64 },
}

/// What the caller does.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// A text frame to the server.
    Text(String),
    /// A binary frame of 16 kHz s16le mono to the server.
    Binary(Vec<u8>),
    /// A binary frame of the server's speech (24 kHz s16le mono) to the player.
    Play(Vec<u8>),
    /// The speech is over: let the player drain and close.
    PlayEnd,
    /// Drop what is queued in the player (a cancel).
    PlayFlush,
    Event(Event),
}

/// The turn is busy.
#[derive(Debug, PartialEq, Eq)]
pub struct Busy;

struct Share {
    key: u32,
    id: String,
    t0: Instant,
}

pub struct Session {
    phase: u8,
    purpose: Purpose,
    end_queued: bool,
    hold: Vec<Vec<u8>>,
    held: usize,
    notes: Notes,
    shares: Vec<Share>,
    next_key: u32,
    audio: bool,
    t_end: Option<Instant>,
    seen_transcript: bool,
    seen_text: bool,
    seen_audio: bool,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Session {
        Session {
            phase: rules::IDLE,
            purpose: Purpose::Command,
            end_queued: false,
            hold: Vec::new(),
            held: 0,
            notes: rules::NO_NOTES,
            shares: Vec::new(),
            next_key: 1,
            audio: false,
            t_end: None,
            seen_transcript: false,
            seen_text: false,
            seen_audio: false,
        }
    }

    pub fn phase(&self) -> u8 {
        self.phase
    }

    pub fn purpose(&self) -> Purpose {
        self.purpose
    }

    /// Notes sent and not yet answered.
    pub fn pending_notes(&self) -> usize {
        self.shares.len()
    }

    /// Whether the microphone should be open: a turn is starting or streaming.
    pub fn wants_audio(&self) -> bool {
        self.phase == rules::STARTING || self.phase == rules::STREAMING
    }

    /// The share ids and the table agree (a check for tests).
    pub fn consistent(&self) -> bool {
        let mut keys: Vec<u32> = self.shares.iter().map(|s| s.key).collect();
        let mut table: Vec<u32> = [self.notes.k0, self.notes.k1, self.notes.k2, self.notes.k3].into_iter().filter(|k| *k != 0).collect();
        keys.sort_unstable();
        table.sort_unstable();
        keys == table
    }

    fn reset_turn(&mut self) {
        self.end_queued = false;
        self.hold.clear();
        self.held = 0;
        self.audio = false;
        self.t_end = None;
        self.seen_transcript = false;
        self.seen_text = false;
        self.seen_audio = false;
    }

    /// The owner pressed. Only an idle client starts a turn.
    pub fn press(&mut self, start: &Start) -> Result<Vec<Action>, Busy> {
        if self.phase != rules::IDLE {
            return Err(Busy);
        }
        self.reset_turn();
        self.phase = rules::step(self.phase, rules::EV_PRESS);
        self.purpose = start.purpose;
        Ok(vec![Action::Text(start.to_json())])
    }

    /// A frame of 16 kHz mono from the decimator: sent while streaming, held while starting, dropped otherwise.
    pub fn frame(&mut self, bytes: Vec<u8>) -> Vec<Action> {
        match rules::frame_action(self.phase) {
            rules::FRAME_SEND => bytes.chunks(MAX_FRAME).map(|c| Action::Binary(c.to_vec())).collect(),
            rules::FRAME_HOLD => {
                let room = rules::hold_room(self.held, bytes.len(), HOLD_CAP);
                if room > 0 {
                    self.held += room;
                    self.hold.push(bytes[..room].to_vec());
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// The owner let go. While the `start` has no answer yet the end is remembered and goes after the held audio.
    pub fn end(&mut self, now: Instant) -> Vec<Action> {
        if self.phase == rules::STARTING {
            self.end_queued = true;
            self.t_end = Some(now);
            return Vec::new();
        }
        if self.phase == rules::STREAMING {
            self.phase = rules::step(self.phase, rules::EV_END);
            self.t_end = Some(now);
            return vec![Action::Text(msg::END.to_owned())];
        }
        Vec::new()
    }

    /// Esc: drop the turn. A note already stored is not touched.
    pub fn cancel(&mut self) -> Vec<Action> {
        if self.phase == rules::IDLE {
            return Vec::new();
        }
        self.phase = rules::step(self.phase, rules::EV_CANCEL);
        self.reset_turn();
        vec![Action::Text(msg::CANCEL.to_owned()), Action::PlayFlush]
    }

    /// The socket closed: the turn and the watching are over (the notes are in the chat all the same).
    pub fn closed(&mut self) -> Vec<Action> {
        let was = self.phase;
        self.phase = rules::step(self.phase, rules::EV_ERROR);
        self.reset_turn();
        self.notes = rules::NO_NOTES;
        self.shares.clear();
        let mut out = vec![Action::PlayFlush];
        if was != rules::IDLE {
            out.push(Action::Event(Event::Failed { code: "closed".into(), message: "The connection to Lucina was lost.".into() }));
        }
        out
    }

    fn span(&self, from: Option<Instant>, now: Instant, what: &'static str) -> Option<Action> {
        from.map(|t| Action::Event(Event::Timing { what, ms: now.saturating_duration_since(t).as_millis() as u64 }))
    }

    fn drop_share(&mut self, id: &str) -> Option<Share> {
        let i = self.shares.iter().position(|s| s.id == id)?;
        let share = self.shares.remove(i);
        self.notes = rules::notes_remove(self.notes, share.key);
        Some(share)
    }

    /// One message from the server.
    pub fn server(&mut self, m: Server, now: Instant) -> Vec<Action> {
        let mut out = Vec::new();
        let live = self.phase != rules::IDLE;
        match m {
            Server::Listening { codec, .. } => {
                if self.phase != rules::STARTING {
                    return out;
                }
                if self.purpose == Purpose::Note && codec.is_none() {
                    self.phase = rules::step(self.phase, rules::EV_CANCEL);
                    self.reset_turn();
                    out.push(Action::Text(msg::CANCEL.to_owned()));
                    out.push(Action::Event(Event::OldServer));
                    return out;
                }
                self.phase = rules::step(self.phase, rules::EV_LISTENING);
                out.push(Action::Event(Event::Listening));
                for frame in std::mem::take(&mut self.hold) {
                    out.extend(frame.chunks(MAX_FRAME).map(|c| Action::Binary(c.to_vec())));
                }
                self.held = 0;
                if self.end_queued {
                    self.end_queued = false;
                    self.phase = rules::step(self.phase, rules::EV_END);
                    out.push(Action::Text(msg::END.to_owned()));
                }
            }
            Server::Endpoint => {
                if self.phase == rules::STREAMING {
                    self.phase = rules::step(self.phase, rules::EV_ENDPOINT);
                    self.t_end.get_or_insert(now);
                    out.push(Action::Event(Event::Endpoint));
                }
            }
            Server::Transcript { text } if live => {
                if !self.seen_transcript {
                    self.seen_transcript = true;
                    out.extend(self.span(self.t_end, now, "end to transcript"));
                }
                out.push(Action::Event(Event::Transcript(text)));
            }
            Server::Delta { text } if live => {
                if !self.seen_text {
                    self.seen_text = true;
                    out.extend(self.span(self.t_end, now, "end to first text"));
                }
                out.push(Action::Event(Event::Delta(text)));
            }
            Server::Answer { text, replaces } if live => {
                if !self.seen_text {
                    self.seen_text = true;
                    out.extend(self.span(self.t_end, now, "end to first text"));
                }
                out.push(Action::Event(Event::Answer { text, replaces }));
            }
            Server::Audio { rate } if live => {
                self.audio = rate == 24_000;
            }
            Server::Reply { text, share_id: None, .. } if live => {
                out.extend(self.span(self.t_end, now, "end to reply"));
                out.push(Action::Event(Event::Reply(text)));
            }
            Server::Reply { text, share_id: Some(id), item_id } => {
                if let Some(s) = self.shares.iter().find(|s| s.id == id) {
                    out.extend(self.span(Some(s.t0), now, "end to note reply"));
                    out.push(Action::Event(Event::NoteReply { share_id: id, text, item_id }));
                }
            }
            Server::Done { share_id: None, .. } => {
                if live {
                    let before = self.phase;
                    self.phase = rules::step(self.phase, rules::EV_DONE);
                    if self.phase == rules::IDLE && before != rules::IDLE {
                        if self.audio {
                            out.push(Action::PlayEnd);
                        }
                        self.reset_turn();
                        out.push(Action::Event(Event::Done));
                    }
                }
            }
            Server::Done { share_id: Some(id), .. } => {
                if self.drop_share(&id).is_some() {
                    out.push(Action::Event(Event::NoteDone { share_id: id }));
                }
            }
            Server::Error { code, message, share_id: None } => {
                if live {
                    let had_audio = self.audio;
                    self.phase = rules::step(self.phase, rules::EV_ERROR);
                    self.reset_turn();
                    if had_audio {
                        out.push(Action::PlayEnd);
                    }
                    out.push(Action::Event(Event::Failed { code, message }));
                }
            }
            Server::Error { code, message, share_id: Some(id) } => {
                if self.drop_share(&id).is_some() {
                    out.push(Action::Event(Event::NoteFailed { share_id: id, code, message }));
                }
            }
            Server::Note { share_id, user_item_id } => {
                if self.phase == rules::WAITING && self.purpose == Purpose::Note && !self.shares.iter().any(|s| s.id == share_id) {
                    let key = self.next_key;
                    self.next_key = if key == u32::MAX { 1 } else { key + 1 };
                    let (table, evicted) = rules::notes_add(self.notes, key);
                    self.notes = table;
                    if evicted != 0 {
                        if let Some(i) = self.shares.iter().position(|s| s.key == evicted) {
                            let gone = self.shares.remove(i);
                            out.push(Action::Event(Event::NoteDropped { share_id: gone.id }));
                        }
                    }
                    out.extend(self.span(self.t_end, now, "end to note"));
                    self.shares.push(Share { key, id: share_id.clone(), t0: self.t_end.unwrap_or(now) });
                    self.phase = rules::step(self.phase, rules::EV_NOTE);
                    self.reset_turn();
                    out.push(Action::Event(Event::NoteStored { share_id, user_item_id }));
                }
            }
            _ => {}
        }
        out
    }

    /// A binary frame from the server: speech, once `audio` has said so.
    pub fn binary(&mut self, bytes: Vec<u8>, now: Instant) -> Vec<Action> {
        if !self.audio || self.phase == rules::IDLE {
            return Vec::new();
        }
        let mut out = Vec::new();
        if !self.seen_audio {
            self.seen_audio = true;
            out.extend(self.span(self.t_end, now, "end to first audio"));
        }
        out.push(Action::Play(bytes));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn t(n: u64) -> Instant {
        // A fixed origin keeps spans deterministic.
        static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *ORIGIN.get_or_init(Instant::now) + Duration::from_millis(n)
    }

    fn listening(codec: bool) -> Server {
        Server::Listening { purpose: None, codec: codec.then(|| "pcm16".to_owned()), rate: None, turn: None }
    }

    fn events(a: &[Action]) -> Vec<&Event> {
        a.iter().filter_map(|x| if let Action::Event(e) = x { Some(e) } else { None }).collect()
    }

    fn start_streaming(s: &mut Session, start: &Start) {
        s.press(start).unwrap();
        s.server(listening(true), t(0));
        assert_eq!(s.phase(), rules::STREAMING);
    }

    #[test]
    fn one_turn_at_a_time() {
        let mut s = Session::new();
        assert!(s.press(&Start::note()).is_ok());
        assert_eq!(s.press(&Start::command()), Err(Busy));
        s.server(listening(true), t(0));
        assert_eq!(s.press(&Start::command()), Err(Busy));
        s.end(t(10));
        assert_eq!(s.press(&Start::command()), Err(Busy));
    }

    #[test]
    fn frames_go_only_while_streaming_and_the_first_ones_are_held() {
        let mut s = Session::new();
        assert!(s.frame(vec![1; 1280]).is_empty(), "idle drops");
        s.press(&Start::note()).unwrap();
        assert!(s.frame(vec![1; 1280]).is_empty(), "starting holds");
        assert!(s.frame(vec![2; 1280]).is_empty());
        let a = s.server(listening(true), t(0));
        let sent: Vec<_> = a.iter().filter(|x| matches!(x, Action::Binary(_))).collect();
        assert_eq!(sent.len(), 2, "the held audio goes first, in order");
        assert_eq!(s.frame(vec![3; 1280]), vec![Action::Binary(vec![3; 1280])]);
        s.end(t(100));
        assert!(s.frame(vec![4; 1280]).is_empty(), "waiting drops");
    }

    #[test]
    fn the_hold_has_a_cap() {
        let mut s = Session::new();
        s.press(&Start::command()).unwrap();
        for _ in 0..200 {
            s.frame(vec![0; 1280]);
        }
        let a = s.server(listening(true), t(0));
        let bytes: usize = a.iter().map(|x| if let Action::Binary(b) = x { b.len() } else { 0 }).sum();
        assert_eq!(bytes, HOLD_CAP);
    }

    #[test]
    fn an_end_before_listening_goes_after_the_held_audio() {
        let mut s = Session::new();
        s.press(&Start::note()).unwrap();
        s.frame(vec![1; 1280]);
        assert!(s.end(t(5)).is_empty());
        let a = s.server(listening(true), t(30));
        let last = a.last().unwrap();
        assert_eq!(*last, Action::Text(msg::END.to_owned()));
        assert!(matches!(a[a.len() - 2], Action::Binary(_)));
        assert_eq!(s.phase(), rules::WAITING);
    }

    #[test]
    fn a_note_turn_ends_with_the_note_and_is_followed_by_shareid() {
        let mut s = Session::new();
        start_streaming(&mut s, &Start::note());
        s.end(t(1000));
        let a = s.server(Server::Transcript { text: "ciao".into() }, t(1083));
        assert!(events(&a).contains(&&Event::Timing { what: "end to transcript", ms: 83 }));
        let a = s.server(Server::Note { share_id: "s1".into(), user_item_id: Some("itm_1".into()) }, t(1400));
        assert!(events(&a).contains(&&Event::NoteStored { share_id: "s1".into(), user_item_id: Some("itm_1".into()) }));
        assert_eq!(s.phase(), rules::IDLE, "the device is free");
        assert_eq!(s.pending_notes(), 1);
        // A new turn may start while the note is answered.
        s.press(&Start::command()).unwrap();
        let a = s.server(Server::Reply { text: "Hi".into(), share_id: Some("s1".into()), item_id: Some("itm_2".into()) }, t(3000));
        assert!(events(&a).iter().any(|e| matches!(e, Event::NoteReply { share_id, .. } if share_id == "s1")));
        assert_eq!(s.phase(), rules::STARTING, "a note's reply does not touch the open turn");
        let a = s.server(Server::Done { share_id: Some("s1".into()), ms: vec![] }, t(3001));
        assert!(events(&a).contains(&&Event::NoteDone { share_id: "s1".into() }));
        assert_eq!(s.pending_notes(), 0);
        assert!(s.consistent());
    }

    #[test]
    fn every_pending_note_is_removed_by_its_done_or_error() {
        let mut s = Session::new();
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            start_streaming(&mut s, &Start::note());
            s.end(t(i as u64));
            s.server(Server::Note { share_id: (*id).into(), user_item_id: None }, t(i as u64 + 1));
        }
        assert_eq!(s.pending_notes(), 3);
        s.server(Server::Error { code: "lucina_unavailable".into(), message: String::new(), share_id: Some("b".into()) }, t(9));
        assert_eq!(s.pending_notes(), 2);
        s.server(Server::Done { share_id: Some("a".into()), ms: vec![] }, t(10));
        s.server(Server::Done { share_id: Some("c".into()), ms: vec![] }, t(11));
        assert_eq!(s.pending_notes(), 0);
        assert!(s.consistent());
        // An unknown id changes nothing.
        assert!(s.server(Server::Done { share_id: Some("zz".into()), ms: vec![] }, t(12)).is_empty());
    }

    #[test]
    fn a_fifth_note_drops_the_oldest() {
        let mut s = Session::new();
        let mut dropped = Vec::new();
        for i in 0..5u64 {
            start_streaming(&mut s, &Start::note());
            s.end(t(i));
            let a = s.server(Server::Note { share_id: format!("n{i}"), user_item_id: None }, t(i + 1));
            for e in events(&a) {
                if let Event::NoteDropped { share_id } = e {
                    dropped.push(share_id.clone());
                }
            }
            assert!(s.consistent());
        }
        assert_eq!(dropped, vec!["n0".to_owned()]);
        assert_eq!(s.pending_notes(), 4);
        // The dropped note's later messages are not ours any more.
        assert!(s.server(Server::Done { share_id: Some("n0".into()), ms: vec![] }, t(20)).is_empty());
    }

    #[test]
    fn an_old_server_gets_a_cancel_and_the_owner_a_reason() {
        let mut s = Session::new();
        s.press(&Start::note()).unwrap();
        let a = s.server(listening(false), t(0));
        assert_eq!(a[0], Action::Text(msg::CANCEL.to_owned()));
        assert!(events(&a).contains(&&Event::OldServer));
        assert_eq!(s.phase(), rules::IDLE);
        // A command works on an old server: it needs nothing new.
        s.press(&Start::command()).unwrap();
        s.server(listening(false), t(1));
        assert_eq!(s.phase(), rules::STREAMING);
    }

    #[test]
    fn a_command_turn_streams_text_and_speech_then_done() {
        let mut s = Session::new();
        start_streaming(&mut s, &Start::command());
        let a = s.server(Server::Endpoint, t(2000));
        assert!(events(&a).contains(&&Event::Endpoint));
        assert_eq!(s.phase(), rules::WAITING);
        assert!(s.frame(vec![0; 1280]).is_empty(), "no audio after the endpoint");
        s.server(Server::Transcript { text: "che ore sono".into() }, t(2080));
        let a = s.server(Server::Delta { text: "Sono".into() }, t(2600));
        assert!(events(&a).contains(&&Event::Timing { what: "end to first text", ms: 600 }));
        assert!(s.binary(vec![1, 2], t(2650)).is_empty(), "no speech before `audio`");
        s.server(Server::Audio { rate: 24000 }, t(2700));
        let a = s.binary(vec![1, 2], t(2710));
        assert_eq!(a[0], Action::Event(Event::Timing { what: "end to first audio", ms: 710 }));
        assert_eq!(a[1], Action::Play(vec![1, 2]));
        s.server(Server::Answer { text: "Sono le tre.".into(), replaces: true }, t(2800));
        s.server(Server::Reply { text: "Sono le tre.".into(), share_id: None, item_id: None }, t(2900));
        let a = s.server(Server::Done { share_id: None, ms: vec![] }, t(3000));
        assert_eq!(a, vec![Action::PlayEnd, Action::Event(Event::Done)]);
        assert_eq!(s.phase(), rules::IDLE);
    }

    #[test]
    fn errors_cancel_and_a_closed_socket_end_the_turn() {
        let mut s = Session::new();
        start_streaming(&mut s, &Start::command());
        let a = s.server(Server::Error { code: "stt_failed".into(), message: "x".into(), share_id: None }, t(1));
        assert!(events(&a).iter().any(|e| matches!(e, Event::Failed { code, .. } if code == "stt_failed")));
        assert_eq!(s.phase(), rules::IDLE);

        start_streaming(&mut s, &Start::note());
        let a = s.cancel();
        assert_eq!(a[0], Action::Text(msg::CANCEL.to_owned()));
        assert_eq!(s.phase(), rules::IDLE);
        assert!(s.cancel().is_empty(), "nothing to cancel");

        start_streaming(&mut s, &Start::note());
        s.end(t(5));
        s.server(Server::Note { share_id: "k".into(), user_item_id: None }, t(6));
        assert!(s.cancel().is_empty(), "a stored note is not touched");
        assert_eq!(s.pending_notes(), 1);
        start_streaming(&mut s, &Start::command());
        let a = s.closed();
        assert!(events(&a).iter().any(|e| matches!(e, Event::Failed { code, .. } if code == "closed")));
        assert_eq!(s.phase(), rules::IDLE);
        assert_eq!(s.pending_notes(), 0);
        assert!(s.consistent());
    }

    #[test]
    fn stray_messages_do_nothing() {
        let mut s = Session::new();
        assert!(s.server(Server::Transcript { text: "x".into() }, t(0)).is_empty());
        assert!(s.server(Server::Done { share_id: None, ms: vec![] }, t(0)).is_empty());
        assert!(s.server(Server::Endpoint, t(0)).is_empty());
        assert!(s.server(Server::Note { share_id: "q".into(), user_item_id: None }, t(0)).is_empty());
        assert!(s.server(Server::Unknown, t(0)).is_empty());
        assert!(s.binary(vec![0; 4], t(0)).is_empty());
        assert_eq!(s.phase(), rules::IDLE);
    }
}
