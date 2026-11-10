//! The device tree walker against the laptop's own DTB (the path in LUCINA_GBGO_DTB). Without it every test prints
//! "skipped: no LUCINA_GBGO_DTB" and passes.

use lucina_modem_rpmh::tree::consumers;

fn dtb() -> Option<Vec<u8>> {
    let path = std::env::var("LUCINA_GBGO_DTB").ok()?;
    std::fs::read(path).ok()
}

#[test]
fn prints_every_consumer() {
    let Some(bytes) = dtb() else {
        println!("skipped: no LUCINA_GBGO_DTB");
        return;
    };
    let fdt = fdt::Fdt::new(&bytes).unwrap();
    let cs = consumers(&fdt);
    for c in &cs {
        println!("{} {} {}", c.node, c.domain, c.state);
    }
    assert!(!cs.is_empty());
}

#[test]
fn usb_holds_cx_at_nominal() {
    let Some(bytes) = dtb() else {
        println!("skipped: no LUCINA_GBGO_DTB");
        return;
    };
    let fdt = fdt::Fdt::new(&bytes).unwrap();
    let cs = consumers(&fdt);
    let usb = cs.iter().find(|c| c.node.ends_with("usb@a6f8800")).expect("usb@a6f8800 is a consumer");
    assert_eq!(usb.domain, 0, "SC7180_CX");
    assert_eq!(usb.state, 256);
}

#[test]
fn gcc_holds_cx_with_no_level() {
    let Some(bytes) = dtb() else {
        println!("skipped: no LUCINA_GBGO_DTB");
        return;
    };
    let fdt = fdt::Fdt::new(&bytes).unwrap();
    let cs = consumers(&fdt);
    let gcc = cs
        .iter()
        .find(|c| c.node.ends_with("clock-controller@100000"))
        .expect("clock-controller@100000 is a consumer");
    assert_eq!(gcc.domain, 0, "SC7180_CX");
    assert_eq!(gcc.state, 0);
}

#[test]
fn self_voting_nodes_are_absent() {
    let Some(bytes) = dtb() else {
        println!("skipped: no LUCINA_GBGO_DTB");
        return;
    };
    let fdt = fdt::Fdt::new(&bytes).unwrap();
    let cs = consumers(&fdt);
    for path in ["remoteproc@4080000", "remoteproc@62400000", "video-codec@aa00000"] {
        assert!(!cs.iter().any(|c| c.node.contains(path)), "{path} is self-voting");
    }
}

#[test]
fn nothing_above_nominal() {
    let Some(bytes) = dtb() else {
        println!("skipped: no LUCINA_GBGO_DTB");
        return;
    };
    let fdt = fdt::Fdt::new(&bytes).unwrap();
    let cs = consumers(&fdt);
    for c in &cs {
        assert!(c.state <= 256, "{} asks for {}", c.node, c.state);
    }
}
