//! lucina-devd lists an app's intents by running `<program> --intents`, and drops the app's intents when that fails (on the laptop,
//! generation 30, Video took the flag for a file to play and never answered).

#[test]
fn the_program_lists_its_intents_and_the_manifest_names_them() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_lucina-app-player"))
        .arg("--intents")
        .env_remove("LUCINA_INTENT")
        .output()
        .expect("the program runs");
    assert!(out.status.success(), "{out:?}");
    let list: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON on stdout");
    let mut offered: Vec<&str> = list.as_array().unwrap().iter().map(|i| i["name"].as_str().unwrap()).collect();
    offered.sort_unstable();
    for intent in list.as_array().unwrap() {
        if let Some(inverse) = intent.get("inverse").and_then(|i| i.as_str()) {
            assert!(offered.contains(&inverse), "inverse intent {inverse} is not in the list");
        }
    }
    let manifest = lucina_app::manifest::Manifest::parse(include_str!("../manifest.app")).unwrap();
    let mut named: Vec<&str> = manifest.intents.iter().map(String::as_str).collect();
    named.sort_unstable();
    assert_eq!(offered, named);
}
