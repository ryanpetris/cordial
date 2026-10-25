use super::*;

#[test]
fn exhausted_candidate_ids_report_the_retained_result_without_losing_candidates() {
    for retained in [false, true] {
        let mut app = Application::new(Build {
            development: false,
            version: "test",
            board: "test_board",
            default_adapter_name: "Test adapter",
            adapter_id: "adapter".into(),
            profile_memory_budget: None,
            bootloader: None,
        });
        if retained {
            let peer = Peer {
                address: [1; 6],
                random: false,
                transport: Transport::Classic,
            };
            let id = app.candidate_id().unwrap();
            app.candidates.push(Candidate {
                id,
                peer,
                address: peer,
                kind: DeviceKind::Keyboard,
                name: "Keyboard".into(),
                rssi: None,
                dirty: true,
            });
        }
        app.candidate_seq = u32::MAX;
        assert_eq!(app.candidate_id(), None);
        let done = p::ScanDone {
            count: app.candidates.len() as u32,
            error: app.scan_result(None),
        };
        assert_eq!(done.count, u32::from(retained));
        assert_eq!(
            done.error,
            Some(p::Error {
                code: p::ErrorCode::Internal as i32,
                reason: p::ErrorReason::ScanCandidates as i32,
                outcome: if retained {
                    p::ErrorOutcome::Partial
                } else {
                    p::ErrorOutcome::NoEffect
                } as i32,
                related: Vec::new(),
            })
        );
        if retained {
            assert_eq!(app.candidates[0].id, 1);
        }
    }
}

#[test]
fn error_conversion_preserves_context_and_unknown_outcomes_for_every_code() {
    for code in [Error::Timeout, Error::InternalError, Error::StorageFailed] {
        let error = wire::error(code, Some(p::ErrorReason::Radio), true);
        assert_eq!(error.reason(), p::ErrorReason::Radio);
        assert_eq!(error.outcome(), p::ErrorOutcome::Unknown);
        assert_eq!(
            wire::error(code, None, false).outcome(),
            p::ErrorOutcome::Unspecified
        );
    }
}
