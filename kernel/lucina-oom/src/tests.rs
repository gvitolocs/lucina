use super::*;

const TOTAL: u64 = 945_000; // 3.69 GB of 4 KiB pages, the laptop

#[test]
fn the_biggest_process_is_chosen() {
    let shell = points(20_000, ADJ_MIN, TOTAL);
    let browser = points(800_000, 0, TOTAL);
    let small = points(5_000, 0, TOTAL);
    assert_eq!(shell, None);
    assert_eq!(choose(&[shell, browser, small]), Some(1));
}

#[test]
fn adj_moves_a_process_up_or_down() {
    // +500 adds half the machine: a small process with it beats a big one without.
    let small_but_marked = points(10_000, 500, TOTAL);
    let big = points(400_000, 0, TOTAL);
    assert!(small_but_marked > big);
    assert_eq!(choose(&[big, small_but_marked]), Some(1));
    // -999 is still a candidate when nothing else is.
    let almost = points(10_000, -999, TOTAL);
    assert!(almost.is_some());
    assert_eq!(choose(&[None, almost, None]), Some(1));
}

#[test]
fn nobody_is_chosen_when_all_are_protected() {
    assert_eq!(choose(&[None, None]), None);
    assert_eq!(choose(&[]), None);
}

#[test]
fn equal_scores_go_to_the_last() {
    let a = points(1000, 0, TOTAL);
    assert_eq!(choose(&[a, a, a]), Some(2));
}

#[test]
fn the_reserve_stays_free() {
    assert!(user_may_take(DEFAULT_RESERVE_PAGES + 1, DEFAULT_RESERVE_PAGES));
    assert!(!user_may_take(DEFAULT_RESERVE_PAGES, DEFAULT_RESERVE_PAGES));
    assert!(!user_may_take(0, DEFAULT_RESERVE_PAGES));
}

#[test]
fn protection_follows_the_program_not_the_parent() {
    assert!(is_protected("/sbin/lucina-sshd"));
    assert!(is_protected("lucina-shell"));
    assert!(!is_protected("/usr/bin/lucinajs"));
    assert!(!is_protected("/sbin/lucina-sshd-helper"));
    // An SSH session's shell inherits -1000 from lucina-sshd: it starts at 0.
    assert_eq!(adj_at_exec(ADJ_MIN, false), 0);
    assert_eq!(adj_at_exec(0, true), ADJ_MIN);
    // A value somebody set and that is not full protection is kept.
    assert_eq!(adj_at_exec(-500, false), -500);
    assert_eq!(adj_at_exec(300, false), 300);
}

#[test]
fn the_reserve_comes_from_the_command_line() {
    assert_eq!(reserve_from_cmdline("a b -- c"), DEFAULT_RESERVE_PAGES);
    assert_eq!(reserve_from_cmdline("lucina.oom.reserve=16 -- x"), 16 * 256);
    assert_eq!(reserve_from_cmdline("-- lucina.oom.reserve=16"), DEFAULT_RESERVE_PAGES);
    assert_eq!(reserve_from_cmdline("lucina.oom.reserve=lots"), DEFAULT_RESERVE_PAGES);
}

#[test]
fn scores_stay_in_range_at_the_limits() {
    assert_eq!(points(rules::LIMIT, ADJ_MAX, rules::LIMIT), Some((rules::LIMIT + rules::LIMIT / 1000 * 1000) as i64));
    assert_eq!(points(0, -999, rules::LIMIT), Some(-999 * (rules::LIMIT / 1000) as i64));
}
