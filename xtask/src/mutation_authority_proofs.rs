use super::{Authority, Event, Origin, Purpose};

#[kani::proof]
#[kani::unwind(1)]
fn rust_exit_protocol_preserves_every_terminal_outcome_count() {
    let survivors: usize = kani::any();
    let timeouts: usize = kani::any();
    let code: Option<i32> = kani::any();
    let expected = crate::mutation::cargo_mutants_expected_exit(survivors, timeouts);
    assert_eq!(expected == 0, survivors == 0 && timeouts == 0);
    assert_eq!(expected == 2, survivors > 0 && timeouts == 0);
    assert_eq!(expected == 3, timeouts > 0);
    let complete = crate::mutation::cargo_mutants_completed(code, survivors, timeouts);
    assert_eq!(
        complete,
        (survivors == 0 && timeouts == 0 && code == Some(0))
            || (survivors > 0 && timeouts == 0 && code == Some(2))
    );
    kani::cover!(complete && survivors == 0);
    kani::cover!(complete && survivors == usize::MAX);
    kani::cover!(!complete && timeouts == usize::MAX);
}

#[kani::proof]
#[kani::unwind(2)]
fn candidate_audits_never_authorize_protected_main_execution() {
    let origin = match kani::any::<u8>() % 3 {
        0 => Origin::Main,
        1 => Origin::Branch,
        _ => Origin::Other,
    };
    let event = match kani::any::<u8>() % 3 {
        0 => Event::Manual,
        1 => Event::Schedule,
        _ => Event::Other,
    };
    let context = Authority {
        origin,
        event,
        immutable_controller: kani::any(),
        target_is_controller: kani::any(),
        owner_dispatch: kani::any(),
    };
    let candidate = context.permits(Purpose::CandidateAudit);
    let protected = context.permits(Purpose::ProtectedMain);
    if candidate {
        assert_eq!(origin, Origin::Branch);
        assert_eq!(event, Event::Manual);
        assert!(context.immutable_controller);
        assert!(context.target_is_controller);
        assert!(context.owner_dispatch);
        assert!(!protected);
    }
    if protected {
        assert_eq!(origin, Origin::Main);
        assert!(matches!(event, Event::Manual | Event::Schedule));
        assert!(context.immutable_controller);
        assert!(!candidate);
    }
    assert_eq!(
        candidate,
        origin == Origin::Branch
            && event == Event::Manual
            && context.immutable_controller
            && context.target_is_controller
            && context.owner_dispatch
    );
    assert_eq!(
        protected,
        origin == Origin::Main
            && matches!(event, Event::Manual | Event::Schedule)
            && context.immutable_controller
    );
    kani::cover!(candidate);
    kani::cover!(protected);
    kani::cover!(!candidate && !protected);
}
