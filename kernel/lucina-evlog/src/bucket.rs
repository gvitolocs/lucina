//! Crash signatures. Windows Error Reporting groups repeats by a bucket of positional parameters (P1..P10: app, version,
//! module, offset, exception code) that nobody can read without the docs. Ours are named inputs with a label in words, and
//! they leave out what changes from build to build (addresses, line numbers, sizes in a reason), so the same fault on two
//! builds is one bucket with two builds in it.

use std::collections::BTreeMap;

use crate::event::{Bucket, Event};

/// FNV-1a, 64 bits: small, stable, no dependency.
pub fn fnv64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Takes out of a reason what differs between builds and runs: hexadecimal numbers become `#x`, a run of 3 or more digits
/// becomes `#`, a source line `:123` becomes `:#`, whitespace runs become one space. Short numbers stay ("CPU 6" is not
/// kept anyway: the reason holds no CPU). At most the first 3 lines and 160 characters.
pub fn normalize(reason: &str) -> String {
    let text: String = reason.lines().take(3).collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '0'
            && chars.get(i + 1) == Some(&'x')
            && chars.get(i + 2).is_some_and(|c| c.is_ascii_hexdigit())
        {
            i += 2;
            while chars.get(i).is_some_and(|c| c.is_ascii_hexdigit()) {
                i += 1;
            }
            out.push_str("#x");
            continue;
        }
        if c == ':' && chars.get(i + 1).is_some_and(|c| c.is_ascii_digit()) {
            i += 1;
            while chars.get(i).is_some_and(|c| c.is_ascii_digit()) {
                i += 1;
            }
            out.push_str(":#");
            continue;
        }
        if c.is_ascii_digit()
            && !out
                .chars()
                .last()
                .is_some_and(|p| p.is_ascii_alphanumeric())
        {
            let start = i;
            while chars.get(i).is_some_and(|c| c.is_ascii_digit()) {
                i += 1;
            }
            if i - start >= 3 {
                out.push('#');
            } else {
                out.extend(&chars[start..i]);
            }
            continue;
        }
        if c.is_whitespace() {
            if !out.ends_with(' ') {
                out.push(' ');
            }
            i += 1;
            continue;
        }
        if c.is_ascii_hexdigit()
            && !out
                .chars()
                .last()
                .is_some_and(|p| p.is_ascii_alphanumeric())
        {
            // A bare 16-digit address.
            let start = i;
            while chars.get(i).is_some_and(|c| c.is_ascii_hexdigit()) {
                i += 1;
            }
            if i - start >= 8 && !chars.get(i).is_some_and(|c| c.is_ascii_alphanumeric()) {
                out.push_str("#x");
            } else {
                out.extend(&chars[start..i]);
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    let out = out.trim().to_string();
    out.chars().take(160).collect()
}

/// A bucket from named inputs. The id hashes `name=value` lines in the order given.
pub fn make(inputs: &[(&str, String)], label: String) -> Bucket {
    let mut text = String::new();
    for (k, v) in inputs {
        text.push_str(k);
        text.push('=');
        text.push_str(v);
        text.push('\n');
    }
    Bucket {
        id: format!("{:016x}", fnv64(text.as_bytes())),
        label,
        inputs: inputs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    }
}

/// One bucket's history: Reliability Monitor's "problem reports" grouped as WER groups them, with builds named.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Group {
    pub bucket: Bucket,
    pub count: u32,
    pub first_ms: Option<i64>,
    pub last_ms: Option<i64>,
    pub first_record: u64,
    pub last_record: u64,
    pub boots: Vec<u32>,
    pub builds: Vec<String>,
}

/// Groups events by bucket, most recent group first.
pub fn groups(events: &[Event]) -> Vec<Group> {
    let mut by: BTreeMap<String, Group> = BTreeMap::new();
    for e in events {
        let Some(b) = &e.bucket else { continue };
        let g = by.entry(b.id.clone()).or_insert_with(|| Group {
            bucket: b.clone(),
            count: 0,
            first_ms: e.time_ms,
            last_ms: e.time_ms,
            first_record: e.record,
            last_record: e.record,
            boots: Vec::new(),
            builds: Vec::new(),
        });
        g.count += 1;
        g.first_ms = match (g.first_ms, e.time_ms) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        g.last_ms = g.last_ms.max(e.time_ms);
        g.first_record = g.first_record.min(e.record);
        g.last_record = g.last_record.max(e.record);
        if let Some(boot) = e.boot {
            if !g.boots.contains(&boot) {
                g.boots.push(boot);
            }
        }
        if let Some(build) = &e.build {
            if !g.builds.contains(build) {
                g.builds.push(build.clone());
            }
        }
    }
    let mut out: Vec<Group> = by.into_values().collect();
    out.sort_by(|a, b| b.last_record.cmp(&a.last_record));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_lose_what_changes_between_builds() {
        assert_eq!(
            normalize(
                "Heap allocation error, layout = Layout {\n    size: 0x40000,\n    align: 1 (1 << 0),\n}"
            ),
            "Heap allocation error, layout = Layout { size: #x, align: 1 (1 << 0),"
        );
        assert_eq!(
            normalize("entered unreachable code at ostd/src/task/mod.rs:210"),
            "entered unreachable code at ostd/src/task/mod.rs:#"
        );
        assert_eq!(
            normalize("fault at ffffffffa08cfe40 on CPU 6"),
            "fault at #x on CPU 6"
        );
        assert_eq!(normalize("took 12345 ms"), "took # ms");
        assert_eq!(normalize("sha deadbeef1"), "sha #x");
        assert_eq!(normalize("abc"), "abc");
    }

    #[test]
    fn same_inputs_same_bucket() {
        let a = make(
            &[("kind", "panic".into()), ("reason", "x".into())],
            "a".into(),
        );
        let b = make(
            &[("kind", "panic".into()), ("reason", "x".into())],
            "b".into(),
        );
        let c = make(
            &[("kind", "panic".into()), ("reason", "y".into())],
            "a".into(),
        );
        assert_eq!(a.id, b.id);
        assert_ne!(a.id, c.id);
        assert_eq!(a.id.len(), 16);
    }
}
