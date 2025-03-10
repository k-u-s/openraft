use maplit::btreeset;
use pretty_assertions::assert_eq;

use crate::core::ServerState;
use crate::engine::testing::Config;
use crate::engine::Command;
use crate::engine::Engine;
use crate::engine::LogIdList;
use crate::entry::EntryRef;
use crate::error::InitializeError;
use crate::error::NotAMembershipEntry;
use crate::error::NotAllowed;
use crate::error::NotInMembers;
use crate::raft::VoteRequest;
use crate::raft_state::LogStateReader;
use crate::EntryPayload;
use crate::LeaderId;
use crate::LogId;
use crate::Membership;
use crate::MetricsChangeFlags;
use crate::Vote;

#[test]
fn test_initialize_single_node() -> anyhow::Result<()> {
    let eng = || {
        let mut eng = Engine::<u64, ()>::default();
        eng.state.enable_validate = false; // Disable validation for incomplete state

        eng.state.server_state = eng.calc_server_state();
        eng
    };

    let log_id0 = LogId {
        leader_id: LeaderId::new(0, 0),
        index: 0,
    };

    let log_id = |term, index| LogId {
        leader_id: LeaderId::new(term, 1),
        index,
    };

    let m1 = || Membership::<u64, ()>::new(vec![btreeset! {1}], None);
    let payload = EntryPayload::<Config>::Membership(m1());
    let mut entries = [EntryRef::new(&payload)];

    tracing::info!("--- ok: init empty node 1 with membership(1,2)");
    tracing::info!("--- expect OK result, check output commands and state changes");
    {
        let mut eng = eng();
        eng.config.id = 1;

        eng.initialize(&mut entries)?;

        assert_eq!(Some(log_id0), eng.state.get_log_id(0));
        assert_eq!(Some(log_id(1, 1)), eng.state.get_log_id(1));
        assert_eq!(Some(log_id(1, 1)), eng.state.last_log_id().copied());

        assert_eq!(ServerState::Leader, eng.state.server_state);
        assert_eq!(
            MetricsChangeFlags {
                // Command::UpdateReplicationStreams will set this flag.
                // Although there is no replication to create.
                replication: true,
                local_data: true,
                cluster: true,
            },
            eng.output.metrics_flags
        );
        assert_eq!(m1(), eng.state.membership_state.effective.membership);

        assert_eq!(
            vec![
                Command::AppendInputEntries { range: 0..1 },
                Command::UpdateMembership {
                    membership: eng.state.membership_state.effective.clone()
                },
                // When update the effective membership, the engine set it to Follower.
                // But when initializing, it will switch to Candidate at once, in the last output command.
                Command::MoveInputCursorBy { n: 1 },
                Command::SaveVote {
                    vote: Vote {
                        term: 1,
                        node_id: 1,
                        committed: false,
                    },
                },
                // TODO: duplicated SaveVote: one is emitted by elect(), the second is emitted when the node becomes
                //       leader.
                Command::SaveVote {
                    vote: Vote {
                        term: 1,
                        node_id: 1,
                        committed: true,
                    },
                },
                Command::BecomeLeader,
                Command::UpdateReplicationStreams { targets: vec![] },
                Command::AppendBlankLog {
                    log_id: LogId {
                        leader_id: LeaderId { term: 1, node_id: 1 },
                        index: 1,
                    },
                },
                Command::ReplicateCommitted {
                    committed: Some(LogId {
                        leader_id: LeaderId { term: 1, node_id: 1 },
                        index: 1,
                    },),
                },
                Command::LeaderCommit {
                    already_committed: None,
                    upto: LogId {
                        leader_id: LeaderId { term: 1, node_id: 1 },
                        index: 1,
                    },
                },
                Command::ReplicateEntries {
                    upto: Some(LogId {
                        leader_id: LeaderId { term: 1, node_id: 1 },
                        index: 1,
                    },),
                }
            ],
            eng.output.commands
        );
    }
    Ok(())
}

#[test]
fn test_initialize() -> anyhow::Result<()> {
    let eng = || {
        let mut eng = Engine::<u64, ()>::default();
        eng.state.enable_validate = false; // Disable validation for incomplete state

        eng.state.server_state = eng.calc_server_state();
        eng
    };

    let log_id0 = LogId {
        leader_id: LeaderId::new(0, 0),
        index: 0,
    };
    let vote0 = Vote::new(0, 0);

    let m12 = || Membership::<u64, ()>::new(vec![btreeset! {1,2}], None);
    let payload = EntryPayload::<Config>::Membership(m12());
    let mut entries = [EntryRef::new(&payload)];

    tracing::info!("--- ok: init empty node 1 with membership(1,2)");
    tracing::info!("--- expect OK result, check output commands and state changes");
    {
        let mut eng = eng();
        eng.config.id = 1;

        eng.initialize(&mut entries)?;

        assert_eq!(Some(log_id0), eng.state.get_log_id(0));
        assert_eq!(None, eng.state.get_log_id(1));
        assert_eq!(Some(log_id0), eng.state.last_log_id().copied());

        assert_eq!(ServerState::Candidate, eng.state.server_state);
        assert_eq!(
            MetricsChangeFlags {
                replication: false,
                local_data: true,
                cluster: true,
            },
            eng.output.metrics_flags
        );
        assert_eq!(m12(), eng.state.membership_state.effective.membership);

        assert_eq!(
            vec![
                Command::AppendInputEntries { range: 0..1 },
                Command::UpdateMembership {
                    membership: eng.state.membership_state.effective.clone()
                },
                // When update the effective membership, the engine set it to Follower.
                // But when initializing, it will switch to Candidate at once, in the last output command.
                Command::MoveInputCursorBy { n: 1 },
                Command::SaveVote {
                    vote: Vote {
                        term: 1,
                        node_id: 1,
                        committed: false,
                    },
                },
                Command::SendVote {
                    vote_req: VoteRequest {
                        vote: Vote {
                            term: 1,
                            node_id: 1,
                            committed: false,
                        },
                        last_log_id: Some(LogId {
                            leader_id: LeaderId { term: 0, node_id: 0 },
                            index: 0,
                        },),
                    },
                },
                Command::InstallElectionTimer { can_be_leader: true },
            ],
            eng.output.commands
        );
    }

    tracing::info!("--- not allowed because of last_log_id");
    {
        let mut eng = eng();
        eng.state.log_ids = LogIdList::new(vec![log_id0]);

        assert_eq!(
            Err(InitializeError::NotAllowed(NotAllowed {
                last_log_id: Some(log_id0),
                vote: vote0,
            })),
            eng.initialize(&mut entries)
        );
    }

    tracing::info!("--- not allowed because of vote");
    {
        let mut eng = eng();
        eng.state.vote = Vote::new(0, 1);

        assert_eq!(
            Err(InitializeError::NotAllowed(NotAllowed {
                last_log_id: None,
                vote: Vote::new(0, 1),
            })),
            eng.initialize(&mut entries)
        );
    }

    tracing::info!("--- node id 0 is not in membership");
    {
        let mut eng = eng();

        assert_eq!(
            Err(InitializeError::NotInMembers(NotInMembers {
                node_id: 0,
                membership: m12()
            })),
            eng.initialize(&mut entries)
        );
    }

    tracing::info!("--- log entry is not a membership entry");
    {
        let mut eng = eng();

        let payload = EntryPayload::<Config>::Blank;
        let mut entries = [EntryRef::new(&payload)];

        assert_eq!(
            Err(InitializeError::NotAMembershipEntry(NotAMembershipEntry {})),
            eng.initialize(&mut entries)
        );
    }

    Ok(())
}
