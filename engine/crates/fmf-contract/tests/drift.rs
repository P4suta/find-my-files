//! The committed generated artifacts (EngineContract.g.cs and docs/contract.md)
//! must each equal a fresh generation — this runs inside `cargo test
//! --workspace`, so the ordinary test gate (and the lefthook pre-push) catches
//! a contract edit whose C# radiation or Markdown reference was not
//! regenerated (ADR-0018).

#[test]
fn generated_artifacts_match_the_contract() {
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_gen-contract"))
        .arg("--check")
        .status()
        .expect("run gen-contract --check");
    assert!(
        status.success(),
        "a generated contract artifact drifted — run `just contract-gen` and commit"
    );
}
