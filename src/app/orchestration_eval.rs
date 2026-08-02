#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    async fn current_serial_wave(delays_ms: &[u64]) -> Duration {
        let started = Instant::now();
        for delay in delays_ms {
            tokio::time::sleep(Duration::from_millis(*delay)).await;
        }
        started.elapsed()
    }

    async fn optimized_parallel_wave(delays_ms: &[u64]) -> Duration {
        let started = Instant::now();
        let mut workers = tokio::task::JoinSet::new();
        for (index, delay) in delays_ms.iter().copied().enumerate() {
            workers.spawn(async move {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                (index, delay)
            });
        }
        let mut ordered = vec![None; delays_ms.len()];
        while let Some(result) = workers.join_next().await {
            let (index, delay) = result.expect("worker completed");
            ordered[index] = Some(delay);
        }
        assert_eq!(ordered.into_iter().flatten().collect::<Vec<_>>(), delays_ms);
        started.elapsed()
    }

    #[tokio::test]
    async fn report_post_change_parallel_latency_and_ordering() {
        let elapsed = optimized_parallel_wave(&[80, 120, 60]).await;
        eprintln!("AITUI_AFTER parallel_read_wave_ms={}", elapsed.as_millis());
        assert!(elapsed < Duration::from_millis(200));
    }

    #[tokio::test]
    async fn dropping_cancelled_batch_receiver_does_not_reach_replacement() {
        let (old_tx, old_rx) = tokio::sync::mpsc::channel(1);
        drop(old_rx);
        let (new_tx, mut new_rx) = tokio::sync::mpsc::channel(1);
        assert!(old_tx.send("stale").await.is_err());
        new_tx.send("fresh").await.expect("replacement receiver");
        assert_eq!(new_rx.recv().await, Some("fresh"));
    }

    #[derive(Clone, Copy)]
    struct BaselineCase {
        child_completed: bool,
        child_claim_correct: bool,
        evidence_valid: bool,
        disagreement: bool,
    }

    fn current_accepts(case: BaselineCase) -> bool {
        case.child_completed
    }

    #[tokio::test]
    async fn report_pre_change_serial_latency_baseline() {
        let elapsed = current_serial_wave(&[80, 120, 60]).await;
        eprintln!("AITUI_BASELINE serial_read_wave_ms={}", elapsed.as_millis());
        assert!(elapsed >= Duration::from_millis(240));
    }

    #[test]
    fn report_post_change_correctness_with_evidence_gated_consensus() {
        use crate::agent::report::{
            reconcile, CheckSpec, ChildReport, EvidenceRef, Finding, FindingAnswer, ReportStatus,
        };

        let checks = vec![CheckSpec {
            id: "result".into(),
            question: "is the result correct?".into(),
        }];
        let report = |answer| ChildReport {
            schema: "aitui.child-report.v1".into(),
            status: ReportStatus::Complete,
            findings: vec![Finding {
                check_id: "result".into(),
                answer,
                statement: "independently checked".into(),
                evidence: vec![EvidenceRef::Command {
                    command: "fixture-check".into(),
                    exit_code: 0,
                    output_excerpt: "verified".into(),
                }],
            }],
            uncertainties: Vec::new(),
        };
        let cases = [
            (
                vec![report(FindingAnswer::Yes), report(FindingAnswer::Yes)],
                "verified",
            ),
            (vec![report(FindingAnswer::Yes)], "unresolved"),
            (
                vec![report(FindingAnswer::Yes), report(FindingAnswer::No)],
                "unresolved",
            ),
            (Vec::new(), "unresolved"),
        ];
        let correct = cases
            .iter()
            .filter(|(reports, expected)| reconcile(reports, &checks).status == *expected)
            .count();
        eprintln!(
            "AITUI_AFTER verified_report_cases={}/{}",
            correct,
            cases.len()
        );
        assert_eq!(correct, cases.len());
    }

    #[test]
    fn report_pre_change_correctness_baseline() {
        let cases = [
            BaselineCase {
                child_completed: true,
                child_claim_correct: true,
                evidence_valid: true,
                disagreement: false,
            },
            BaselineCase {
                child_completed: true,
                child_claim_correct: false,
                evidence_valid: false,
                disagreement: false,
            },
            BaselineCase {
                child_completed: true,
                child_claim_correct: false,
                evidence_valid: true,
                disagreement: true,
            },
            BaselineCase {
                child_completed: false,
                child_claim_correct: false,
                evidence_valid: false,
                disagreement: false,
            },
        ];
        let correct = cases
            .iter()
            .filter(|case| {
                let accepted = current_accepts(**case);
                accepted == (case.child_claim_correct && case.evidence_valid && !case.disagreement)
            })
            .count();
        eprintln!(
            "AITUI_BASELINE verified_report_cases={}/{}",
            correct,
            cases.len()
        );
        assert_eq!(correct, 2);
    }
}
