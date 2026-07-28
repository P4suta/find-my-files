//! The committed C# binding must equal a fresh generation. This runs inside
//! the canonical nextest workspace run, so the ordinary test gate catches a contract edit
//! whose C# radiation was not regenerated (ADR-0018).

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
