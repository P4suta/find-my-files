#![no_main]
//! Fuzz the query pipeline over arbitrary text: `parse` → `compile`, plus the
//! whole-query `regex:` mode. Query text crosses the privilege boundary (the
//! non-elevated UI hands it to the elevated service), so the parser, the AST
//! compiler, and the regex builder must never panic, hang, or blow the regex
//! size caps into an abort — a malformed query is a clean `Err`.

use fmf_core::query::{self, CaseMode, RegexScope, UtcResolver};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    // text → AST → compiled matchers (UtcResolver is the pure, OS-independent
    // date resolver; the Windows one is cfg-gated out on the Linux fuzzer).
    if let Ok(ast) = query::parse(text) {
        let _ = query::compile(&ast, CaseMode::Smart, &UtcResolver);
    }

    // Whole-query regex mode: the user string is fed straight to the regex
    // builder, which must reject pathological patterns via the size/DFA caps
    // rather than panic or run away (docs/SECURITY.md threat 5).
    let _ = query::compile_whole_regex(text, CaseMode::Smart, RegexScope::Name);
    let _ = query::compile_whole_regex(text, CaseMode::Smart, RegexScope::Path);
});
