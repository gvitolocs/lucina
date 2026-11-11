//! Credential values out of tool output: every span `lucina_account::secrets::detect` finds is replaced by
//! `[secret:<kind>]`, and the kinds found are returned with the text.

/// `text` with every credential value replaced by `[secret:<kind>]`, plus the kinds found.
pub fn redact(text: &str) -> (String, Vec<String>) {
    let spans = lucina_account::secrets::detect(text);
    let mut out = String::new();
    let mut kinds: Vec<String> = Vec::new();
    let mut last = 0;
    for &(start, end) in &spans {
        let name = lucina_account::secrets::suggest_name(&text[start..end]);
        let kind = if name.is_empty() { "token" } else { name };
        out.push_str(&text[last..start]);
        out.push_str("[secret:");
        out.push_str(kind);
        out.push(']');
        if !kinds.iter().any(|k| k == kind) {
            kinds.push(kind.to_owned());
        }
        last = end;
    }
    out.push_str(&text[last..]);
    (out, kinds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_api_key_and_a_github_token_are_replaced() {
        let key = format!("sk-{}", "a1B2c3D4".repeat(4));
        let gh = format!("ghp_{}", "aB1".repeat(8));
        let text = format!("key: {key}\ntoken: {gh}\nall good");
        let (redacted, kinds) = redact(&text);
        assert_eq!(
            redacted,
            "key: [secret:OPENAI_API_KEY]\ntoken: [secret:GITHUB_TOKEN]\nall good"
        );
        assert_eq!(kinds, ["OPENAI_API_KEY", "GITHUB_TOKEN"]);
    }

    #[test]
    fn other_text_is_unchanged() {
        let text = "the build finished in 42 seconds, nothing to hide";
        let (redacted, kinds) = redact(text);
        assert_eq!(redacted, text);
        assert!(kinds.is_empty());
    }
}
