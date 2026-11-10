//! The chat's cards on the DSi: the server's timeline items and the ones made here (a message being sent, a voice note
//! being uploaded, a reply that came in a POST's answer), in one order.
//!
//! The rule: a card made here sorts after every server item (`local_ts`) until the server's copy of it arrives, and that
//! copy then updates the card in place (its time, its words, a voice note's transcript). v1.59 on the DSi gave local
//! cards no time at all, so they sorted as the oldest and fell off the screen as soon as the next page arrived.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::api::Item;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Card {
    pub id: String,
    pub lucina: bool,
    pub text: String,
    /// A voice note's length. A voice note shows no words (the owner, 2026-11-08: only the note, its length and its
    /// waveform), so its `text` stays empty.
    pub voice_ms: Option<u32>,
    /// A voice note's waveform strip (0..=255 a byte, at most 64).
    pub wave: Vec<u8>,
    pub pending: bool,
    pub failed: bool,
    /// Sensitive: drawn as a line saying it is on the phone.
    pub hidden: bool,
    pub others: u32,
    /// The server's ISO time, or `local_ts` for a card made here.
    pub ts: String,
}

/// A time for a card made here: after any server time ("2026-…" < "~"), in the order the cards were made.
pub fn local_ts(seq: u32) -> String {
    format!("~{seq:010}")
}

/// What a card says: the item's text, and nothing for a voice note (the server titles a note with its file name,
/// "📎 voice-note.wav"; the owner wants only the note itself).
fn words(it: &Item) -> String {
    if it.voice.is_some() {
        String::new()
    } else {
        it.text.clone()
    }
}

/// Merges a page of server items into `cards` and keeps the newest `keep`, oldest first.
pub fn merge(cards: &mut Vec<Card>, items: Vec<Item>, keep: usize) {
    for it in items {
        // the server's copy of a message sent from here: by its client message id, or by the id the POST gave it
        let at = it
            .client_message_id
            .as_ref()
            .and_then(|cm| cards.iter().position(|c| c.id == *cm))
            .or_else(|| cards.iter().position(|c| c.id == it.id));
        match at {
            Some(i) => {
                let c = &mut cards[i];
                c.id = it.id.clone();
                c.pending = false;
                c.failed = false;
                if !it.ts.is_empty() {
                    c.ts = it.ts.clone();
                }
                let w = words(&it);
                if !w.is_empty() {
                    c.text = w;
                }
                if let Some((ms, _)) = it.voice {
                    if ms > 0 || c.voice_ms.is_none() {
                        c.voice_ms = Some(ms);
                    }
                    c.text.clear();
                }
                if !it.wave.is_empty() {
                    c.wave = it.wave.clone();
                }
                c.hidden = it.sensitive;
                c.others = it.others;
            }
            None => cards.push(Card {
                id: it.id.clone(),
                lucina: it.lucina,
                text: words(&it),
                voice_ms: it.voice.as_ref().map(|(ms, _)| *ms),
                wave: it.wave.clone(),
                pending: false,
                failed: false,
                hidden: it.sensitive,
                others: it.others,
                ts: it.ts.clone(),
            }),
        }
    }
    sort_and_trim(cards, keep);
}

/// Oldest first by time (stable: cards made here keep the order they were made in), the newest `keep` kept.
pub fn sort_and_trim(cards: &mut Vec<Card>, keep: usize) {
    cards.sort_by(|a, b| a.ts.cmp(&b.ts));
    let n = cards.len();
    if n > keep {
        cards.drain(..n - keep);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, lucina: bool, text: &str, ts: &str) -> Item {
        Item { id: id.into(), lucina, text: text.into(), ts: ts.into(), ..Item::default() }
    }

    #[test]
    fn cards_made_here_stay_newest_and_take_the_servers_time() {
        let mut cards = Vec::new();
        merge(&mut cards, std::vec![item("1", true, "old a", "2026-11-08T10:00:00Z"), item("2", true, "old b", "2026-11-08T11:00:00Z")], 40);
        // a voice note uploaded and answered here (ids from the share's answer), before any refresh
        cards.push(Card { id: "u9".into(), voice_ms: Some(2959), ts: local_ts(1), ..Card::default() });
        cards.push(Card { id: "l9".into(), lucina: true, text: "Got it.".into(), ts: local_ts(2), ..Card::default() });
        sort_and_trim(&mut cards, 40);
        // the next page: an older item, and the server's copies of the two
        let mut voice = item("u9", false, "", "2026-11-08T17:33:00Z");
        voice.voice = Some((2959, Some("remind me to buy milk".into())));
        merge(&mut cards, std::vec![item("0", true, "older", "2026-11-08T09:00:00Z"), voice, item("l9", true, "Got it.", "2026-11-08T17:33:05Z")], 40);
        let ids: Vec<&str> = cards.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["0", "1", "2", "u9", "l9"]);
        // a voice note shows no words: not its transcript, not the server's file-name title
        assert_eq!(cards[3].text, "");
        assert_eq!(cards[3].voice_ms, Some(2959));
    }

    #[test]
    fn a_voice_note_never_shows_its_file_name() {
        let mut v = item("v1", false, "📎 voice-note.wav", "2026-11-08T17:44:16Z");
        v.voice = Some((8393, None));
        v.wave = std::vec![10, 200, 30];
        let mut cards = Vec::new();
        merge(&mut cards, std::vec![v.clone()], 40);
        assert_eq!((cards[0].text.as_str(), cards[0].voice_ms, &cards[0].wave[..]), ("", Some(8393), &[10u8, 200, 30][..]));
        // and the card made here, matched by the server's copy, stays without words and keeps its own strip
        let mut cards = std::vec![Card { id: "v1".into(), voice_ms: Some(8393), wave: std::vec![1, 2], pending: true, ts: local_ts(1), ..Card::default() }];
        v.wave.clear();
        merge(&mut cards, std::vec![v], 40);
        assert_eq!((cards[0].text.as_str(), &cards[0].wave[..], cards[0].pending), ("", &[1u8, 2][..], false));
    }

    #[test]
    fn a_pending_message_is_replaced_by_its_server_copy_and_trim_keeps_the_newest() {
        let mut cards = std::vec![Card { id: "fortuna-abc".into(), text: "hi".into(), pending: true, ts: local_ts(3), ..Card::default() }];
        let mut it = item("44", false, "hi", "2026-11-08T17:40:00Z");
        it.client_message_id = Some("fortuna-abc".into());
        let mut page: Vec<Item> = (0..45).map(|k| item(&std::format!("x{k}"), true, "y", &std::format!("2026-11-08T08:{k:02}:00Z"))).collect();
        page.push(it);
        merge(&mut cards, page, 40);
        assert_eq!(cards.len(), 40);
        let last = cards.last().unwrap();
        assert_eq!((last.id.as_str(), last.pending), ("44", false));
        assert!(cards.iter().all(|c| c.id != "fortuna-abc"));
    }
}
