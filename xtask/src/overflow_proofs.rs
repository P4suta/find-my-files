use super::{boot_follows_preparation, build_args, sequence, Variant, ROUNDS};

#[kani::proof]
#[kani::unwind(2)]
fn a_measurement_boot_follows_preparation_and_is_not_in_the_future() {
    let prepared: u64 = kani::any();
    let boot: u64 = kani::any();
    let now: u64 = kani::any();
    let accepted = boot_follows_preparation(prepared, boot, now);
    assert_eq!(accepted, prepared < boot && boot <= now);
    kani::cover!(accepted);
    kani::cover!(!accepted && boot <= prepared);
    kani::cover!(!accepted && boot > now);
}

#[kani::proof]
#[kani::unwind(8)]
fn every_round_contains_each_variant_in_the_required_order() {
    let index: usize = kani::any();
    kani::assume(index < ROUNDS);
    let round = sequence()[index];
    let expected = if index % 2 == 0 {
        [Variant::A, Variant::B, Variant::C]
    } else {
        [Variant::C, Variant::B, Variant::A]
    };
    assert_eq!(round, expected);
    kani::cover!(index == 0);
    kani::cover!(index == 1);
    kani::cover!(index == ROUNDS - 1);
}

#[kani::proof]
#[kani::unwind(128)]
fn profile_arguments_change_only_the_selected_overflow_checks() {
    let selector: u8 = kani::any();
    kani::assume(selector < 3);
    let variant = [Variant::A, Variant::B, Variant::C][usize::from(selector)];
    let expected: &[&str] = match variant {
        Variant::A => &["build", "--locked", "--release", "-p", "fmf-cli"],
        Variant::B => &[
            "build",
            "--locked",
            "--release",
            "-p",
            "fmf-cli",
            "--config",
            "profile.release.package.fmf-core.overflow-checks=true",
            "--config",
            "profile.release.package.fmf-runtime.overflow-checks=true",
        ],
        Variant::C => &[
            "build",
            "--locked",
            "--release",
            "-p",
            "fmf-cli",
            "--config",
            "profile.release.overflow-checks=true",
        ],
    };
    let actual = build_args(variant);
    assert_eq!(actual, expected);
    kani::cover!(selector == 0);
    kani::cover!(selector == 1);
    kani::cover!(selector == 2);
}
