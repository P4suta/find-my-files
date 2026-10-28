#[kani::proof]
#[kani::unwind(130)]
fn product_base_is_the_maximal_ascii_prefix_before_metadata() {
    let bytes: [u8; 64] = kani::any();
    let length: usize = kani::any();
    kani::assume(length <= bytes.len());
    for byte in &bytes[..length] {
        kani::assume(byte.is_ascii());
    }
    let full = core::str::from_utf8(&bytes[..length]).expect("ASCII version");
    let base = super::product_base(full);
    assert!(full.starts_with(base));
    assert!(!base.contains(['-', '+']));
    if base.len() < full.len() {
        assert!(matches!(full.as_bytes()[base.len()], b'-' | b'+'));
    }
    kani::cover!(full.is_empty());
    kani::cover!(full == "1-dev+g");
    kani::cover!(full == "1+g-dev");
}
