//! How the chat shows attachments. A voice note is never named: "Voice message" and its length. Other kinds keep their names.

/// The legacy auto caption `📎 <file names>` the share router wrote for voice parts: true when the item has a voice attachment and
/// every name listed after the clip is a voice attachment's name. A real caption (anything else) stays.
pub fn hides_voice_caption(text: &str, attachments: &[(String, Option<String>)]) -> bool {
    let names: Vec<&str> = attachments
        .iter()
        .filter(|(k, _)| k == "voice")
        .filter_map(|(_, n)| n.as_deref().map(str::trim))
        .filter(|n| !n.is_empty())
        .collect();
    if !attachments.iter().any(|(k, _)| k == "voice") {
        return false;
    }
    let Some(rest) = text.trim().strip_prefix("\u{1F4CE} ") else { return false };
    rest.split(',').map(str::trim).all(|n| !n.is_empty() && names.contains(&n))
}

/// "Voice message · 0:05", or "Voice message" when the length is unknown.
pub fn voice_label(ms: Option<u64>) -> String {
    match ms {
        Some(ms) => {
            let total = (ms + 500) / 1000;
            let (h, m, s) = (total / 3600, total % 3600 / 60, total % 60);
            if h > 0 { format!("Voice message \u{b7} {h}:{m:02}:{s:02}") } else { format!("Voice message \u{b7} {m}:{s:02}") }
        }
        None => "Voice message".to_owned(),
    }
}

/// The line for a non-voice attachment: its name stays.
pub fn file_note(kind: &str, name: Option<&str>) -> String {
    format!("Attachment: {} ({kind}). Open it on your phone.", name.unwrap_or("no name"))
}

/// The voice labels of an item, one per voice attachment.
pub fn voice_labels(item: &lucina_account::timeline::Item) -> Vec<String> {
    item.attachments
        .iter()
        .enumerate()
        .filter(|(_, (k, _))| k == "voice")
        .map(|(i, _)| voice_label(item.attachment_ms.get(i).copied().flatten()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voice(name: &str) -> (String, Option<String>) {
        ("voice".into(), Some(name.into()))
    }

    #[test]
    fn the_legacy_caption_is_hidden() {
        assert!(hides_voice_caption("\u{1F4CE} voice-note.wav", &[voice("voice-note.wav")]));
        assert!(hides_voice_caption("  \u{1F4CE} note.m4a ", &[voice("note.m4a")]));
        assert!(hides_voice_caption("\u{1F4CE} a.wav, b.wav", &[voice("a.wav"), voice("b.wav")]));
    }

    #[test]
    fn a_real_caption_stays() {
        assert!(!hides_voice_caption("Listen to this", &[voice("voice-note.wav")]));
        assert!(!hides_voice_caption("\u{1F4CE} voice-note.wav and more", &[voice("voice-note.wav")]));
        assert!(!hides_voice_caption("\u{1F4CE} ", &[voice("voice-note.wav")]));
    }

    #[test]
    fn other_kinds_keep_their_names() {
        let pdf = ("pdf".to_owned(), Some("report.pdf".to_owned()));
        assert!(!hides_voice_caption("\u{1F4CE} report.pdf", &[pdf.clone()]));
        assert!(!hides_voice_caption("\u{1F4CE} report.pdf, note.m4a", &[pdf, voice("note.m4a")]));
        assert!(file_note("pdf", Some("report.pdf")).contains("report.pdf"));
    }

    #[test]
    fn the_voice_label_has_no_name() {
        assert_eq!(voice_label(Some(5000)), "Voice message \u{b7} 0:05");
        assert_eq!(voice_label(Some(65_400)), "Voice message \u{b7} 1:05");
        assert_eq!(voice_label(None), "Voice message");
    }
}
