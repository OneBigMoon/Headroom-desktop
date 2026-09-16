//! Headless access to the same audit, apply and restore implementation as the UI.
use headroom_desktop_lib::instruction_governance;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result: Result<serde_json::Value, String> = match args.as_slice() {
        [] => instruction_governance::audit_instructions().map(|value| serde_json::json!(value)),
        [command, target, baseline, candidate] if command == "apply" =>
            instruction_governance::apply_instruction_candidate(target.clone(), baseline.clone(), candidate.clone())
                .map(|id| serde_json::json!({"snapshot": id})),
        [command, id] if command == "restore" =>
            instruction_governance::restore_instruction_snapshot(id.clone())
                .map(|()| serde_json::json!({"restored": id})),
        [command] if command == "snapshots" =>
            instruction_governance::list_instruction_snapshots().map(|ids| serde_json::json!(ids)),
        _ => Err("Usage: instruction-audit [apply TARGET BASELINE_SHA256 CANDIDATE_SHA256 | restore SNAPSHOT_ID | snapshots]".into()),
    };
    match result {
        Ok(value) => println!("{}", serde_json::to_string_pretty(&value).unwrap()),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
