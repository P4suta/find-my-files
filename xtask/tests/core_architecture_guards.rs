use std::fs;
use std::path::Path;

use syn::{Attribute, Item, Meta};
use toml_edit::{DocumentMut, Table};

fn assert_no_production_dependencies(table: &Table) {
    for (name, item) in table {
        if matches!(name, "dependencies" | "build-dependencies") {
            assert!(item.as_table_like().expect("dependency table").is_empty());
        } else if name != "dev-dependencies" {
            if let Some(nested) = item.as_table() {
                assert_no_production_dependencies(nested);
            }
        }
    }
}

fn is_test_only(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        if let Meta::List(meta) = &attribute.meta {
            meta.path.is_ident("cfg") && meta.tokens.to_string() == "test"
        } else {
            false
        }
    })
}

fn assert_only_owned_allocation(items: &[Item]) {
    for item in items {
        match item {
            Item::ExternCrate(external) if !is_test_only(&external.attrs) => {
                assert_eq!(
                    external.ident, "alloc",
                    "core cannot import an effect provider"
                );
            }
            Item::Mod(module) if !is_test_only(&module.attrs) => {
                if let Some((_, items)) = &module.content {
                    assert_only_owned_allocation(items);
                }
            }
            _ => {}
        }
    }
}

#[test]
fn core_compiles_without_the_standard_library_or_production_dependencies() {
    let manifest: DocumentMut = include_str!("../../engine/crates/fmf-core/Cargo.toml")
        .parse()
        .expect("core manifest");
    assert_no_production_dependencies(manifest.as_table());
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../engine/crates/fmf-core/src");
    let source = fs::read_to_string(root.join("lib.rs")).expect("core library");
    let library = syn::parse_file(&source).expect("core syntax");
    assert!(library
        .attrs
        .iter()
        .any(|attribute| attribute.path().is_ident("no_std")));
    assert!(source.contains("#![forbid(unsafe_code)]"));
    assert!(source.contains("deny(clippy::panic, clippy::expect_used, clippy::unwrap_used)"));
    let mut pending = vec![root];
    let mut checked = 0;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).expect("core source directory") {
            let entry = entry.expect("source entry");
            let kind = entry.file_type().expect("source kind");
            if kind.is_dir() {
                pending.push(entry.path());
            } else if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "rs")
            {
                let source = fs::read_to_string(entry.path()).expect("core source");
                let parsed = syn::parse_file(&source).expect("core syntax");
                assert_only_owned_allocation(&parsed.items);
                checked += 1;
            }
        }
    }
    assert!(checked > 1, "core source walk must not be empty");
}

#[test]
fn borders_use_the_one_shared_runtime() {
    for source in [
        include_str!("../../engine/crates/fmf-ffi/Cargo.toml"),
        include_str!("../../engine/crates/fmf-service/Cargo.toml"),
        include_str!("../../engine/crates/fmf-cli/Cargo.toml"),
    ] {
        let manifest: DocumentMut = source.parse().expect("boundary manifest");
        assert_eq!(
            manifest["dependencies"]["fmf-runtime"]["workspace"].as_bool(),
            Some(true)
        );
        assert!(manifest["dependencies"].get("fmf-core").is_none());
    }
}
