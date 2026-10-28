#![no_main]
//! Fuzz the query pipeline over arbitrary text: `parse` → `compile`, plus the whole-query `regex:` mode.
//! Query text crosses the privilege boundary (the non-elevated UI hands it to the elevated service), so the parser, the AST compiler, and the regex builder must never panic, hang, or blow the regex size caps into an abort — a malformed query is a clean `Err`.

use fmf_runtime::query::{self, CaseMode, RegexScope, UtcResolver};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    if let Ok(ast) = query::parse(text) {
        assert_eq!(query::parse(&format!(" \t{text}\n")), Ok(ast.clone()));
        let _ = query::compile(&ast, CaseMode::Smart, &UtcResolver);
    }

    let _ = query::compile_whole_regex(text, CaseMode::Smart, RegexScope::Name);
    let _ = query::compile_whole_regex(text, CaseMode::Smart, RegexScope::Path);

    let literal: String = text.chars().filter(char::is_ascii_alphanumeric).collect();
    if !literal.is_empty() {
        let parsed = query::parse(&format!("\"{literal}\"")).expect("quoted ASCII literal");
        assert_eq!(parsed.groups, vec![vec![query::Term::Name(literal)]]);
    }
});
