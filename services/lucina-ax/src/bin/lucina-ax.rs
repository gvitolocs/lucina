//! `lucina-ax tree [target]` and `lucina-ax act <id> <action> <version> [value] [--target t]`: the same calls lucina-devd makes
//! for `ui_tree` and `ui_act` (`lucina_ax::devd::run`), from a shell on the laptop, for tests and debugging.
//! `lucina-ax state <list|fields|read|watch> ['<json args>']`: the same for `state_*` (`lucina_ax::state::run`).

use serde_json::json;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("state") {
        let tool = format!("state_{}", args.get(1).map_or("read", String::as_str));
        let call: serde_json::Value = match args.get(2).map(|a| serde_json::from_str(a)) {
            None => json!({}),
            Some(Ok(v)) => v,
            Some(Err(e)) => {
                eprintln!("lucina-ax: the arguments are not JSON: {e}");
                std::process::exit(2);
            }
        };
        if !lucina_ax::state::NAMES.contains(&tool.as_str()) {
            eprintln!("usage: lucina-ax state <list|fields|read|watch> ['<json args>']");
            std::process::exit(2);
        }
        let out = lucina_ax::state::run(&tool, &call);
        println!("{}", out["text"].as_str().unwrap_or(""));
        eprintln!("ok={}", out["ok"]);
        std::process::exit(if out["ok"] == json!(true) { 0 } else { 1 });
    }
    let (tool, call) = match args.first().map(String::as_str) {
        Some("tree") => ("ui_tree", json!({"target": args.get(1)})),
        Some("act") if args.len() >= 4 => {
            let target = args.iter().position(|a| a == "--target").and_then(|i| args.get(i + 1));
            let value = args.get(4).filter(|v| !v.starts_with("--"));
            ("ui_act", json!({"id": args[1], "action": args[2], "version": args[3], "value": value, "target": target}))
        }
        _ => {
            eprintln!(
                "usage: lucina-ax tree [front|shell|<pid>]\n       lucina-ax act <id> <action> <version> [value] [--target t]\n       lucina-ax state <list|fields|read|watch> ['<json args>']"
            );
            std::process::exit(2);
        }
    };
    // Optional fields that were not given are left out, as devd's schema has them.
    let mut call = call;
    if let Some(m) = call.as_object_mut() {
        m.retain(|_, v| !v.is_null());
    }
    // From a shell on the laptop the owner decides by running it: `--decided` stands for a decision.
    let decided = args.iter().any(|a| a == "--decided");
    let out = lucina_ax::devd::run(tool, &call, decided);
    println!("{}", out["text"].as_str().unwrap_or(""));
    eprintln!("ok={} target={} effect={}", out["ok"], out["target"], out["effect"]);
    std::process::exit(if out["ok"] == json!(true) { 0 } else { 1 });
}
