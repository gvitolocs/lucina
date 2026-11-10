use super::*;

const LAPTOP: &str = "SHELL=/bin/sh loglevel=info console=tty0 lucina.wdt=on lucina.hostname=Juno001 root=/dev/ufs0lun0p2 \
    ro init=/etc/lucina/init.sh -- /etc/lucina/remote.sh /etc/lucina/session.sh";

#[test]
fn without_the_word_the_name_is_lucinaos() {
    let c = from_cmdline("SHELL=/bin/sh loglevel=info console=tty0 -- /etc/lucina/remote.sh sh -l");
    assert_eq!(c, Choice::Default);
    assert_eq!(c.name(), "lucinaos");
    assert_eq!(from_cmdline("").name(), "lucinaos");
}

#[test]
fn the_laptop_is_juno001() {
    assert_eq!(from_cmdline(LAPTOP), Choice::Given("Juno001"));
    assert_eq!(from_cmdline(LAPTOP).name(), "Juno001");
}

#[test]
fn the_last_word_counts() {
    assert_eq!(from_cmdline("lucina.hostname=a lucina.hostname=b").name(), "b");
}

#[test]
fn init_arguments_are_not_the_kernels() {
    assert_eq!(from_cmdline("loglevel=info -- /bin/sh lucina.hostname=Other"), Choice::Default);
}

#[test]
fn a_bad_name_is_refused_and_the_default_stands() {
    for bad in ["", "-juno", "juno-", "Juno_001", "juno.example.internal", "Jünö"] {
        let line = format!("{WORD}{bad}");
        let c = from_cmdline(&line);
        assert_eq!(c.name(), DEFAULT, "{bad:?}");
        assert!(matches!(c, Choice::Refused(_) | Choice::Default), "{bad:?}");
    }
    assert_eq!(from_cmdline("lucina.hostname=Juno_001"), Choice::Refused("Juno_001"));
}

#[test]
fn the_length_bound_is_64() {
    let max = [b'a'; MAX];
    assert!(valid(&max));
    let over = [b'a'; MAX + 1];
    assert!(!valid(&over));
    assert!(valid(b"a"));
    assert!(!valid(b""));
}

#[test]
fn every_byte_is_checked() {
    for b in 0..=255u8 {
        let ok = b.is_ascii_alphanumeric() || b == b'-';
        assert_eq!(rules::allowed(b), ok, "{b:#x}");
        assert_eq!(valid(&[b'a', b, b'z']), ok, "{b:#x} inside");
    }
    assert!(valid(b"a-b"));
    assert!(!valid(b"-"));
}

