use std::borrow::Cow;

use super::canonical_literal;

const ASCII_PROOF_BYTES: usize = 8;
const _: () = assert!(ASCII_PROOF_BYTES <= u8::MAX as usize);

#[kani::proof]
#[kani::unwind(9)]
fn canonical_ascii_literals_borrow_the_original_input() {
    let bytes: [u8; ASCII_PROOF_BYTES] = kani::any();
    let bytes = bytes.map(|byte| byte & 0x7F);
    let length: u8 = kani::any();
    let length = usize::from(length);
    kani::assume(length <= bytes.len());
    let input = std::str::from_utf8(&bytes[..length]).expect("ASCII is valid UTF-8");
    let canonical = canonical_literal(input);
    assert!(matches!(canonical, Cow::Borrowed(_)));
    assert_eq!(canonical.as_ref(), input);
    assert_eq!(canonical.as_ptr(), input.as_ptr());
    kani::cover!(length == 0);
    kani::cover!(length == ASCII_PROOF_BYTES);
}
