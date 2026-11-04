use anyhow::{ensure, Context, Result};
use clap::{Args, ValueEnum};

const COMMIT_HEX_BYTES: usize = 40;

#[derive(Args)]
pub struct CommandArgs {
    #[arg(long, value_enum, default_value_t = Purpose::ProtectedMain)]
    purpose: Purpose,
    #[arg(long)]
    controller_sha: String,
    #[arg(long)]
    target_sha: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Purpose {
    ProtectedMain,
    CandidateAudit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Origin {
    Main,
    Branch,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Event {
    Manual,
    Schedule,
    Other,
}

#[derive(Clone, Copy, Debug)]
struct Authority {
    origin: Origin,
    event: Event,
    immutable_controller: bool,
    target_is_controller: bool,
    owner_dispatch: bool,
}

impl Authority {
    const fn permits(self, purpose: Purpose) -> bool {
        if !self.immutable_controller {
            return false;
        }
        match purpose {
            Purpose::ProtectedMain => {
                matches!(self.origin, Origin::Main)
                    && matches!(self.event, Event::Manual | Event::Schedule)
            }
            Purpose::CandidateAudit => {
                matches!(self.origin, Origin::Branch)
                    && matches!(self.event, Event::Manual)
                    && self.target_is_controller
                    && self.owner_dispatch
            }
        }
    }
}

#[derive(Clone, Copy)]
struct WorkflowContext<'a> {
    head: &'a str,
    workflow: &'a str,
    branch_ref: &'a str,
    event: &'a str,
    actor: &'a str,
    triggering_actor: &'a str,
    owner: &'a str,
}

fn authority(args: &CommandArgs, context: &WorkflowContext<'_>) -> Result<Authority> {
    for sha in [
        args.controller_sha.as_str(),
        args.target_sha.as_str(),
        context.head,
        context.workflow,
    ] {
        ensure!(
            sha.len() == COMMIT_HEX_BYTES
                && sha
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "mutation authority requires exact lowercase 40-hex commits"
        );
    }
    Ok(Authority {
        origin: if context.branch_ref == "refs/heads/main" {
            Origin::Main
        } else if context
            .branch_ref
            .strip_prefix("refs/heads/")
            .is_some_and(|branch| !branch.is_empty())
        {
            Origin::Branch
        } else {
            Origin::Other
        },
        event: match context.event {
            "workflow_dispatch" => Event::Manual,
            "schedule" => Event::Schedule,
            _ => Event::Other,
        },
        immutable_controller: context.head == args.controller_sha
            && context.workflow == args.controller_sha,
        target_is_controller: args.target_sha == args.controller_sha,
        owner_dispatch: !context.owner.is_empty()
            && context.actor.eq_ignore_ascii_case(context.owner)
            && context.triggering_actor.eq_ignore_ascii_case(context.owner),
    })
}

pub fn run(args: CommandArgs) -> Result<()> {
    let read = |name| std::env::var(name).with_context(|| format!("read {name}"));
    let head = read("GITHUB_SHA")?;
    let workflow = read("GITHUB_WORKFLOW_SHA")?;
    let branch_ref = read("GITHUB_REF")?;
    let event = read("GITHUB_EVENT_NAME")?;
    let actor = read("GITHUB_ACTOR")?;
    let triggering_actor = read("GITHUB_TRIGGERING_ACTOR")?;
    let owner = read("GITHUB_REPOSITORY_OWNER")?;
    let context = WorkflowContext {
        head: &head,
        workflow: &workflow,
        branch_ref: &branch_ref,
        event: &event,
        actor: &actor,
        triggering_actor: &triggering_actor,
        owner: &owner,
    };
    ensure!(
        authority(&args, &context)?.permits(args.purpose),
        "mutation {:?} authority rejected the workflow context",
        args.purpose
    );
    println!(
        "mutation authority: {:?}; target {}",
        args.purpose, args.target_sha
    );
    Ok(())
}

#[cfg(kani)]
#[path = "mutation_authority_proofs.rs"]
mod kani_proofs;

#[cfg(test)]
mod tests {
    use super::{authority, Authority, CommandArgs, Event, Origin, Purpose, WorkflowContext};

    fn main() -> Authority {
        Authority {
            origin: Origin::Main,
            event: Event::Manual,
            immutable_controller: true,
            target_is_controller: false,
            owner_dispatch: false,
        }
    }

    fn candidate() -> Authority {
        Authority {
            origin: Origin::Branch,
            target_is_controller: true,
            owner_dispatch: true,
            ..main()
        }
    }

    #[test]
    fn a_branch_needs_an_explicit_owner_candidate_audit() {
        assert!(!candidate().permits(Purpose::ProtectedMain));
        assert!(candidate().permits(Purpose::CandidateAudit));
        assert!(!Authority {
            owner_dispatch: false,
            ..candidate()
        }
        .permits(Purpose::CandidateAudit));
    }

    #[test]
    fn candidate_evidence_needs_one_commit_and_a_manual_branch_dispatch() {
        for context in [
            Authority {
                immutable_controller: false,
                ..candidate()
            },
            Authority {
                target_is_controller: false,
                ..candidate()
            },
            Authority {
                event: Event::Schedule,
                ..candidate()
            },
            Authority {
                event: Event::Other,
                ..candidate()
            },
            Authority {
                origin: Origin::Other,
                ..candidate()
            },
            Authority {
                origin: Origin::Main,
                ..candidate()
            },
        ] {
            assert!(!context.permits(Purpose::CandidateAudit), "{context:?}");
        }
    }

    #[test]
    fn protected_main_keeps_its_existing_target_and_actor_contract() {
        assert!(main().permits(Purpose::ProtectedMain));
        assert!(Authority {
            event: Event::Schedule,
            ..main()
        }
        .permits(Purpose::ProtectedMain));
        assert!(!Authority {
            immutable_controller: false,
            ..main()
        }
        .permits(Purpose::ProtectedMain));
        assert!(!Authority {
            event: Event::Other,
            ..main()
        }
        .permits(Purpose::ProtectedMain));
    }

    #[test]
    fn workflow_metadata_binds_the_real_sha_ref_and_both_actors() {
        let sha = "a".repeat(40);
        let different = "b".repeat(40);
        let args = CommandArgs {
            purpose: Purpose::CandidateAudit,
            controller_sha: sha.clone(),
            target_sha: sha.clone(),
        };
        let context = WorkflowContext {
            head: &sha,
            workflow: &sha,
            branch_ref: "refs/heads/audit",
            event: "workflow_dispatch",
            actor: "Owner",
            triggering_actor: "owner",
            owner: "OWNER",
        };
        let permits = |context: WorkflowContext<'_>| {
            authority(&args, &context)
                .expect("canonical metadata")
                .permits(args.purpose)
        };
        assert!(permits(context));
        assert!(!permits(WorkflowContext {
            head: &different,
            ..context
        }));
        assert!(!permits(WorkflowContext {
            workflow: &different,
            ..context
        }));
        assert!(!permits(WorkflowContext {
            actor: "contributor",
            ..context
        }));
        assert!(!permits(WorkflowContext {
            triggering_actor: "contributor",
            ..context
        }));
        assert!(!permits(WorkflowContext {
            owner: "",
            ..context
        }));
        assert!(!permits(WorkflowContext {
            branch_ref: "refs/heads/",
            ..context
        }));
        assert!(!permits(WorkflowContext {
            branch_ref: "refs/tags/audit",
            ..context
        }));
        assert!(authority(
            &args,
            &WorkflowContext {
                head: "abc",
                ..context
            }
        )
        .is_err());
        assert!(authority(
            &args,
            &WorkflowContext {
                workflow: &"A".repeat(40),
                ..context
            }
        )
        .is_err());
    }
}
