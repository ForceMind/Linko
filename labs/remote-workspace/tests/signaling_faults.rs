use linko_workspace_lab::session::{SessionGate, SurfaceKind};
use linko_workspace_lab::signaling::{
    ControlDescription, Direction, MediaParameters, Negotiation, Role,
};
use linko_workspace_lab::{Binding, Error, SurfaceKey};

const SOURCE: [u8; 32] = [1; 32];
const VIEWER: [u8; 32] = [2; 32];

fn gate(nonce: u8) -> SessionGate {
    SessionGate::new(
        SurfaceKey {
            binding: Binding {
                source: SOURCE,
                viewer: VIEWER,
                nonce: [nonce; 16],
            },
            object: 4,
            generation: 1,
        },
        SurfaceKind::Window,
        1,
        false,
    )
    .unwrap()
}

fn descriptions(gate: &SessionGate, generation: u64) -> (ControlDescription, ControlDescription) {
    let offer = ControlDescription {
        surface: gate.key(),
        view_epoch: gate.view_epoch(),
        generation,
        role: Role::Offer,
        parameters: MediaParameters {
            direction: Direction::SendOnly,
            dtls_sha256: [10; 32],
            ice_credentials_digest: [11; 32],
        },
    };
    let answer = ControlDescription {
        role: Role::Answer,
        parameters: MediaParameters {
            direction: Direction::ReceiveOnly,
            dtls_sha256: [12; 32],
            ice_credentials_digest: [13; 32],
        },
        ..offer
    };
    (offer, answer)
}

#[test]
fn rejects_replaced_fingerprints_ice_roles_and_sources() {
    let mut gate = gate(3);
    gate.grant_view(1, 99).unwrap();
    let mut guard = Negotiation::new(&gate, 1).unwrap();
    let generation = guard.begin(&mut gate, 1).unwrap();
    let (mut offer, answer) = descriptions(&gate, generation);

    assert_eq!(
        guard.admit(&gate, VIEWER, &offer, offer.parameters, 2),
        Err(Error::InvalidIdentity)
    );
    offer.surface.object += 1;
    assert_eq!(
        guard.admit(&gate, SOURCE, &offer, offer.parameters, 2),
        Err(Error::StaleTarget)
    );
    offer.surface = gate.key();
    for actual in [
        MediaParameters {
            dtls_sha256: [99; 32],
            ..offer.parameters
        },
        MediaParameters {
            ice_credentials_digest: [99; 32],
            ..offer.parameters
        },
    ] {
        assert_eq!(
            guard.admit(&gate, SOURCE, &offer, actual, 2),
            Err(Error::PermissionDenied)
        );
    }
    offer.parameters.direction = Direction::ReceiveOnly;
    assert_eq!(
        guard.admit(&gate, SOURCE, &offer, offer.parameters, 2),
        Err(Error::PermissionDenied)
    );
    offer.parameters.direction = Direction::SendOnly;
    assert_eq!(
        guard.admit(&gate, VIEWER, &answer, answer.parameters, 2),
        Err(Error::InvalidSequence)
    );

    guard
        .admit(&gate, SOURCE, &offer, offer.parameters, 2)
        .unwrap();
    assert_eq!(
        guard.ready(&gate, generation, 2),
        Err(Error::InvalidSequence)
    );
    guard
        .admit(&gate, VIEWER, &answer, answer.parameters, 2)
        .unwrap();
    assert_eq!(guard.ready(&gate, generation, 2), Ok(()));
    assert_eq!(gate.can_control(2), Err(Error::PermissionDenied));
}

#[test]
fn rejects_old_attempts_after_rebuild_and_old_grants_after_reconsent() {
    let mut gate = gate(3);
    gate.grant_view(1, 99).unwrap();
    let mut guard = Negotiation::new(&gate, 1).unwrap();
    let first = guard.begin(&mut gate, 1).unwrap();
    let (offer, answer) = descriptions(&gate, first);

    guard
        .admit(&gate, SOURCE, &offer, offer.parameters, 2)
        .unwrap();
    assert_eq!(
        guard.admit(&gate, SOURCE, &offer, offer.parameters, 2),
        Err(Error::InvalidSequence)
    );
    guard
        .admit(&gate, VIEWER, &answer, answer.parameters, 2)
        .unwrap();
    let mut restarted = Negotiation::new(&gate, 3).unwrap();
    let second = restarted.begin(&mut gate, 3).unwrap();
    assert!(second > first);
    assert_eq!(guard.ready(&gate, first, 3), Err(Error::InvalidSequence));
    assert_eq!(
        restarted.admit(&gate, SOURCE, &offer, offer.parameters, 3),
        Err(Error::InvalidSequence)
    );
    assert_eq!(
        guard.admit(&gate, VIEWER, &answer, answer.parameters, 3),
        Err(Error::InvalidSequence)
    );

    gate.revoke_all(4).unwrap();
    assert_eq!(
        restarted.ready(&gate, second, 4),
        Err(Error::PermissionDenied)
    );
    gate.grant_view(5, 99).unwrap();
    assert_eq!(guard.begin(&mut gate, 5), Err(Error::StaleTarget));
    let mut replacement = Negotiation::new(&gate, 5).unwrap();
    assert!(replacement.begin(&mut gate, 5).unwrap() > second);
    assert_eq!(
        replacement.admit(&gate, SOURCE, &offer, offer.parameters, 5),
        Err(Error::StaleTarget)
    );
}

#[test]
fn reconnect_expiry_and_monotonic_time_fail_closed() {
    let mut original = gate(3);
    assert!(matches!(
        Negotiation::new(&original, 1),
        Err(Error::PermissionDenied)
    ));
    original.grant_view(1, 10).unwrap();
    let mut guard = Negotiation::new(&original, 1).unwrap();
    let generation = guard.begin(&mut original, 2).unwrap();
    let (offer, answer) = descriptions(&original, generation);
    guard
        .admit(&original, SOURCE, &offer, offer.parameters, 4)
        .unwrap();
    assert_eq!(
        guard.admit(&original, VIEWER, &answer, answer.parameters, 3),
        Err(Error::InvalidTime)
    );
    guard
        .admit(&original, VIEWER, &answer, answer.parameters, 4)
        .unwrap();
    assert_eq!(guard.ready(&original, generation, 10), Ok(()));
    assert_eq!(
        guard.ready(&original, generation, 11),
        Err(Error::PermissionDenied)
    );

    original.disconnect(12).unwrap();
    assert_eq!(
        guard.ready(&original, generation, 12),
        Err(Error::Disconnected)
    );
    let mut renewed = gate(4);
    renewed.grant_view(12, 99).unwrap();
    assert_eq!(guard.begin(&mut renewed, 12), Err(Error::StaleTarget));
    let mut replacement = Negotiation::new(&renewed, 12).unwrap();
    assert_eq!(replacement.begin(&mut renewed, 12), Ok(generation));
    assert_eq!(
        replacement.admit(&renewed, SOURCE, &offer, offer.parameters, 12),
        Err(Error::StaleTarget)
    );
}
