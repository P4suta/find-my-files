#[kani::proof]
#[kani::unwind(65)]
fn product_base_ends_at_the_first_metadata_delimiter() {
    let bytes: [u8; 64] = kani::any();
    let length: usize = kani::any();
    kani::assume(length <= bytes.len());
    let end = super::product_base_end(&bytes[..length]);
    assert!(end <= length);
    let index: usize = kani::any();
    if index < end {
        assert!(!matches!(bytes[index], b'-' | b'+'));
    }
    if end < length {
        assert!(matches!(bytes[end], b'-' | b'+'));
    }
    kani::cover!(length == 0);
    kani::cover!(end == length && length > 0);
    kani::cover!(end < length);
}
