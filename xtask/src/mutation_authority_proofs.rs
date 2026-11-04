use super::{Authority, Event, Origin, Purpose};

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
