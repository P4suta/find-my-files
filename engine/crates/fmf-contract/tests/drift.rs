//! The committed EngineContract.g.cs must equal a fresh generation — this
//! runs inside `cargo test --workspace`, so the ordinary test gate (and the
//! lefthook pre-push) catches a contract edit whose C# radiation was not
//! regenerated (ADR-0018).

#[test]
fn generated_csharp_matches_the_contract() {
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_gen-contract"))
        .arg("--check")
        .status()
        .expect("run gen-contract --check");
    assert!(
        status.success(),
        "EngineContract.g.cs drifted — run `just contract-gen` and commit"
    );
}
