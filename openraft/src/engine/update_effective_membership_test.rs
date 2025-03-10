use std::sync::Arc;

use maplit::btreeset;

use crate::core::ServerState;
use crate::engine::Command;
use crate::engine::Engine;
use crate::progress::entry::ProgressEntry;
use crate::progress::Progress;
use crate::EffectiveMembership;
use crate::LeaderId;
use crate::LogId;
use crate::Membership;
use crate::MembershipState;
use crate::MetricsChangeFlags;
use crate::Vote;

crate::declare_raft_types!(
    pub(crate) Foo: D=(), R=(), NodeId=u64, Node=()
);

fn log_id(term: u64, index: u64) -> LogId<u64> {
    LogId::<u64> {
        leader_id: LeaderId { term, node_id: 1 },
        index,
    }
}

fn m01() -> Membership<u64, ()> {
    Membership::<u64, ()>::new(vec![btreeset! {0,1}], None)
}

fn m23() -> Membership<u64, ()> {
    Membership::<u64, ()>::new(vec![btreeset! {2,3}], None)
}

fn m23_45() -> Membership<u64, ()> {
    Membership::<u64, ()>::new(vec![btreeset! {2,3}], Some(btreeset! {4,5}))
}

fn m34() -> Membership<u64, ()> {
    Membership::<u64, ()>::new(vec![btreeset! {3,4}], None)
}

fn m4_356() -> Membership<u64, ()> {
    Membership::<u64, ()>::new(vec![btreeset! {4}], Some(btreeset! {3,5,6}))
}

fn eng() -> Engine<u64, ()> {
    let mut eng = Engine::default();
    eng.config.id = 2;
    eng.state.membership_state.committed = Arc::new(EffectiveMembership::new(Some(log_id(1, 1)), m01()));
    eng.state.membership_state.effective = Arc::new(EffectiveMembership::new(Some(log_id(2, 3)), m23()));
    eng.state.server_state = eng.calc_server_state();
    eng
}

#[test]
fn test_update_effective_membership_at_index_0_is_allowed() -> anyhow::Result<()> {
    let mut eng = eng();

    eng.update_effective_membership(&log_id(0, 0), &m34());

    assert_eq!(
        MembershipState {
            committed: Arc::new(EffectiveMembership::new(Some(log_id(1, 1)), m01())),
            effective: Arc::new(EffectiveMembership::new(Some(log_id(0, 0)), m34()))
        },
        eng.state.membership_state
    );
    assert_eq!(ServerState::Learner, eng.state.server_state);

    assert_eq!(
        MetricsChangeFlags {
            replication: false,
            local_data: false,
            cluster: true,
        },
        eng.output.metrics_flags
    );

    assert_eq!(
        vec![
            //
            Command::UpdateMembership {
                membership: Arc::new(EffectiveMembership::new(Some(log_id(0, 0)), m34())),
            },
        ],
        eng.output.commands
    );

    Ok(())
}

#[test]
fn test_update_effective_membership_for_leader() -> anyhow::Result<()> {
    let mut eng = eng();
    eng.state.server_state = ServerState::Leader;
    // Make it a real leader: voted for itself and vote is committed.
    eng.state.vote = Vote::new_committed(2, 2);
    eng.new_leader();

    eng.update_effective_membership(&log_id(3, 4), &m34());

    assert_eq!(
        MembershipState {
            committed: Arc::new(EffectiveMembership::new(Some(log_id(1, 1)), m01())),
            effective: Arc::new(EffectiveMembership::new(Some(log_id(3, 4)), m34()))
        },
        eng.state.membership_state
    );
    assert_eq!(
        ServerState::Leader,
        eng.state.server_state,
        "Leader wont be affected by membership change"
    );

    assert_eq!(
        MetricsChangeFlags {
            replication: true,
            local_data: false,
            cluster: true,
        },
        eng.output.metrics_flags
    );

    assert_eq!(
        vec![
            //
            Command::UpdateMembership {
                membership: Arc::new(EffectiveMembership::new(Some(log_id(3, 4)), m34())),
            },
            Command::UpdateReplicationStreams {
                targets: vec![(3, ProgressEntry::empty(0)), (4, ProgressEntry::empty(0))], /* node-2 is leader,
                                                                                            * won't be removed */
            }
        ],
        eng.output.commands
    );

    assert!(
        eng.internal_server_state.leading().unwrap().progress.get(&4).matching.is_none(),
        "exists, but it is a None"
    );

    Ok(())
}

#[test]
fn test_update_effective_membership_update_learner_process() -> anyhow::Result<()> {
    // When updating membership, voter progreess should inherit from learner progress, and learner process should
    // inherit from voter process. If voter changes to learner or vice versa.

    let mut eng = eng();
    eng.state.server_state = ServerState::Leader;
    // Make it a real leader: voted for itself and vote is committed.
    eng.state.vote = Vote::new_committed(2, 2);
    eng.state.membership_state.effective = Arc::new(EffectiveMembership::new(Some(log_id(2, 3)), m23_45()));
    eng.new_leader();

    if let Some(l) = &mut eng.internal_server_state.leading_mut() {
        assert_eq!(&ProgressEntry::empty(0), l.progress.get(&4));
        assert_eq!(&ProgressEntry::empty(0), l.progress.get(&5));

        let p = ProgressEntry::new(Some(log_id(1, 4)));
        let _ = l.progress.update(&4, p);
        assert_eq!(&p, l.progress.get(&4));

        let p = ProgressEntry::new(Some(log_id(1, 5)));
        let _ = l.progress.update(&5, p);
        assert_eq!(&p, l.progress.get(&5));

        let p = ProgressEntry::new(Some(log_id(1, 3)));
        let _ = l.progress.update(&3, p);
        assert_eq!(&p, l.progress.get(&3));
    } else {
        unreachable!("leader should not be None");
    }

    eng.update_effective_membership(&log_id(3, 4), &m4_356());

    assert_eq!(
        MembershipState {
            committed: Arc::new(EffectiveMembership::new(Some(log_id(1, 1)), m01())),
            effective: Arc::new(EffectiveMembership::new(Some(log_id(3, 4)), m4_356()))
        },
        eng.state.membership_state
    );

    if let Some(l) = &mut eng.internal_server_state.leading_mut() {
        assert_eq!(
            &ProgressEntry::new(Some(log_id(1, 4))),
            l.progress.get(&4),
            "learner-4 progress should be transferred to voter progress"
        );
        assert_eq!(
            &ProgressEntry::new(Some(log_id(1, 3))),
            l.progress.get(&3),
            "voter-3 progress should be transferred to learner progress"
        );

        assert_eq!(
            &ProgressEntry::new(Some(log_id(1, 5))),
            l.progress.get(&5),
            "learner-5 has previous value"
        );
        assert_eq!(
            &ProgressEntry::empty(0), //
            l.progress.get(&6)
        );
    } else {
        unreachable!("leader should not be None");
    }

    Ok(())
}
