use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const MAX_REPORT_BYTES: usize = 64 * 1024;
const MAX_FINDINGS: usize = 32;
const MAX_EVIDENCE_PER_FINDING: usize = 12;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckSpec {
    pub id: String,
    pub question: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FindingAnswer {
    Yes,
    No,
    Mixed,
    Unknown,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Complete,
    Partial,
    Blocked,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EvidenceRef {
    File {
        path: String,
        line_start: usize,
        line_end: usize,
        quote: String,
    },
    Command {
        command: String,
        exit_code: i32,
        output_excerpt: String,
    },
    Web {
        url: String,
        quote: String,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub check_id: String,
    pub answer: FindingAnswer,
    pub statement: String,
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChildReport {
    pub schema: String,
    pub status: ReportStatus,
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub uncertainties: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ConsensusFinding {
    pub check_id: String,
    pub answer: FindingAnswer,
    pub statement: String,
    pub support: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct VerificationSummary {
    pub schema: &'static str,
    pub status: &'static str,
    pub findings: Vec<ConsensusFinding>,
    pub unresolved: Vec<String>,
    /// Per-attempt transport, parsing, and evidence-validation outcomes. Empty
    /// when reconciliation is used directly without running replicas.
    pub diagnostics: Vec<String>,
}

/// Owned, display-oriented form of a final replicated-verification report.
/// Unlike `VerificationSummary`, this is deserialized from persisted/model text.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct VerificationDisplayReport {
    pub schema: String,
    pub status: String,
    #[serde(default)]
    pub findings: Vec<ConsensusFinding>,
    #[serde(default)]
    pub unresolved: Vec<String>,
    #[serde(default)]
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationReportMatch {
    pub start: usize,
    pub end: usize,
    pub report: VerificationDisplayReport,
}

/// Find final verification-summary JSON objects embedded in prose (including
/// the `agent N (completed):\n{...}` hand-off sent back to the parent agent).
pub fn verification_reports(text: &str) -> Vec<VerificationReportMatch> {
    let mut reports = Vec::new();
    let mut search_from = 0;
    while let Some(relative) = text[search_from..].find('{') {
        let start = search_from + relative;
        let slice = &text[start..];
        let mut stream =
            serde_json::Deserializer::from_str(slice).into_iter::<VerificationDisplayReport>();
        match stream.next() {
            Some(Ok(report)) if report.schema == "aitui.verification-summary.v1" => {
                let end = start + stream.byte_offset();
                reports.push(VerificationReportMatch { start, end, report });
                search_from = end.max(start + 1);
            }
            _ => search_from = start + 1,
        }
    }
    reports
}

pub fn verification_report(text: &str) -> Option<VerificationDisplayReport> {
    verification_reports(text)
        .into_iter()
        .next()
        .map(|matched| matched.report)
}

#[allow(
    dead_code,
    reason = "kept as the simple validation API used by tests and external callers"
)]
pub fn parse_and_validate(
    text: &str,
    checks: &[CheckSpec],
    cwd: &Path,
) -> Result<ChildReport, String> {
    parse_and_validate_detailed(text, checks, cwd).map(|(report, _)| report)
}

pub fn parse_and_validate_detailed(
    text: &str,
    checks: &[CheckSpec],
    cwd: &Path,
) -> Result<(ChildReport, Vec<String>), String> {
    if text.len() > MAX_REPORT_BYTES {
        return Err("report exceeds size limit".into());
    }
    let start = text
        .find('{')
        .ok_or_else(|| "report does not contain a JSON object".to_string())?;
    let mut report: ChildReport = serde_json::from_str(&text[start..])
        .map_err(|error| format!("invalid structured report: {}", error))?;
    if report.schema != "aitui.child-report.v1" {
        return Err("unsupported report schema".into());
    }
    if report.findings.len() > MAX_FINDINGS {
        return Err("report has too many findings".into());
    }
    let expected: BTreeSet<_> = checks.iter().map(|check| check.id.as_str()).collect();
    let mut seen = BTreeSet::new();
    for finding in &report.findings {
        if !expected.contains(finding.check_id.as_str()) {
            return Err(format!("unknown check id '{}'", finding.check_id));
        }
        if !seen.insert(finding.check_id.as_str()) {
            return Err(format!("duplicate check id '{}'", finding.check_id));
        }
        if finding.evidence.len() > MAX_EVIDENCE_PER_FINDING {
            return Err(format!("too much evidence for '{}'", finding.check_id));
        }
    }

    let mut warnings = Vec::new();
    report.findings.retain_mut(|finding| {
        let mut evidence_errors = Vec::new();
        finding
            .evidence
            .retain_mut(|evidence| match validate_evidence(evidence, cwd) {
                Ok(()) => true,
                Err(error) => {
                    evidence_errors.push(error);
                    false
                }
            });
        for error in evidence_errors {
            warnings.push(format!("check '{}': {}", finding.check_id, error));
        }
        if !matches!(finding.answer, FindingAnswer::Unknown) && finding.evidence.is_empty() {
            warnings.push(format!(
                "check '{}': finding omitted because no valid evidence remained",
                finding.check_id
            ));
            false
        } else {
            true
        }
    });
    Ok((report, warnings))
}

fn validate_evidence(evidence: &mut EvidenceRef, cwd: &Path) -> Result<(), String> {
    let EvidenceRef::File {
        path,
        line_start,
        line_end,
        quote,
    } = evidence
    else {
        return Ok(());
    };
    if *line_start == 0 || line_end < line_start {
        return Err(format!("invalid line range for '{}'", path));
    }
    let joined = if Path::new(path).is_absolute() {
        PathBuf::from(path.as_str())
    } else {
        cwd.join(path.as_str())
    };
    let canonical_cwd = cwd
        .canonicalize()
        .map_err(|error| format!("cannot resolve cwd: {}", error))?;
    let canonical_path = joined
        .canonicalize()
        .map_err(|error| format!("cannot resolve evidence '{}': {}", path, error))?;
    if !canonical_path.starts_with(&canonical_cwd) {
        return Err(format!("evidence path escapes cwd: '{}'", path));
    }
    let content = std::fs::read_to_string(&canonical_path)
        .map_err(|error| format!("cannot read evidence '{}': {}", path, error))?;
    let lines: Vec<_> = content.lines().collect();
    let quote = quote.trim().to_string();
    if quote.is_empty() {
        return Err(format!("evidence quote is empty for '{}'", path));
    }
    if *line_start > 0 && *line_end >= *line_start && *line_end <= lines.len() {
        let excerpt = lines[*line_start - 1..*line_end].join("\n");
        if quote_matches(&excerpt, &quote) {
            return Ok(());
        }
    }

    // Models frequently retain the correct verbatim text but report a nearby
    // line range after a document changes, or collapse Markdown wrapping into
    // spaces. Relocate only a unique, substantive quote so evidence remains
    // anchored to current file contents rather than accepting paraphrases.
    if normalize_whitespace(&quote).chars().count() >= 16 {
        if let Some((start, end)) = find_unique_quote_range(&lines, &quote)? {
            *line_start = start;
            *line_end = end;
            return Ok(());
        }
    }
    Err(format!("evidence quote is stale for '{}'", path))
}

fn find_unique_quote_range(lines: &[&str], quote: &str) -> Result<Option<(usize, usize)>, String> {
    let mut best: Option<(usize, usize)> = None;
    let mut ambiguous = false;
    for start in 0..lines.len() {
        let max_end = (start + 12).min(lines.len());
        for end in start + 1..=max_end {
            if !quote_matches(&lines[start..end].join("\n"), quote) {
                continue;
            }
            let span = end - start;
            match best {
                None => {
                    best = Some((start + 1, end));
                    ambiguous = false;
                }
                Some((best_start, best_end)) if span < best_end - best_start + 1 => {
                    best = Some((start + 1, end));
                    ambiguous = false;
                }
                Some((best_start, best_end))
                    if span == best_end - best_start + 1
                        && (best_start, best_end) != (start + 1, end) =>
                {
                    ambiguous = true;
                }
                _ => {}
            }
            break;
        }
    }
    if ambiguous {
        Err("evidence quote occurs more than once".into())
    } else {
        Ok(best)
    }
}

fn quote_matches(source: &str, quote: &str) -> bool {
    source.contains(quote) || normalize_whitespace(source).contains(&normalize_whitespace(quote))
}

fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn reconcile(reports: &[ChildReport], checks: &[CheckSpec]) -> VerificationSummary {
    let mut findings = Vec::new();
    let mut unresolved = Vec::new();
    for check in checks {
        let candidates: Vec<_> = reports
            .iter()
            .filter_map(|report| {
                report
                    .findings
                    .iter()
                    .find(|finding| finding.check_id == check.id)
            })
            .collect();
        let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        for finding in &candidates {
            let key = match finding.answer {
                FindingAnswer::Yes => "yes",
                FindingAnswer::No => "no",
                FindingAnswer::Mixed => "mixed",
                FindingAnswer::Unknown => "unknown",
            };
            *counts.entry(key).or_default() += 1;
        }
        let accepted = [
            ("yes", FindingAnswer::Yes),
            ("no", FindingAnswer::No),
            ("mixed", FindingAnswer::Mixed),
            ("unknown", FindingAnswer::Unknown),
        ]
        .into_iter()
        .find(|(key, _)| counts.get(key).copied().unwrap_or(0) >= 2);
        if let Some((key, answer)) = accepted {
            if let Some(source) = candidates.iter().find(|finding| finding.answer == answer) {
                findings.push(ConsensusFinding {
                    check_id: check.id.clone(),
                    answer,
                    statement: source.statement.clone(),
                    support: format!("{}/{} replicas", counts[key], reports.len()),
                    evidence: source.evidence.iter().map(evidence_label).collect(),
                });
            }
        } else {
            unresolved.push(check.id.clone());
        }
    }
    VerificationSummary {
        schema: "aitui.verification-summary.v1",
        status: if unresolved.is_empty() {
            "verified"
        } else if findings.is_empty() {
            "unresolved"
        } else {
            "partially_verified"
        },
        findings,
        unresolved,
        diagnostics: Vec::new(),
    }
}

fn evidence_label(evidence: &EvidenceRef) -> String {
    match evidence {
        EvidenceRef::File {
            path,
            line_start,
            line_end,
            ..
        } => format!("{}:{}-{}", path, line_start, line_end),
        EvidenceRef::Command {
            command, exit_code, ..
        } => {
            format!("command `{}` exited {}", command, exit_code)
        }
        EvidenceRef::Web { url, .. } => url.clone(),
    }
}

pub fn report_instructions(checks: &[CheckSpec]) -> String {
    let checks = serde_json::to_string(checks).unwrap_or_else(|_| "[]".into());
    format!(
        "Return one JSON object, schema aitui.child-report.v1, no markdown. \
         Shape: {{\"schema\":\"aitui.child-report.v1\",\"status\":\"complete|partial|blocked\",\
         \"findings\":[{{\"check_id\":string,\"answer\":\"yes|no|mixed|unknown\",\"statement\":string,\
         \"evidence\":[{{\"kind\":\"file\",\"path\":string,\"line_start\":integer,\"line_end\":integer,\"quote\":string}}]}}],\
         \"uncertainties\":[]}}. Every non-unknown answer needs evidence. For file evidence, quote a short \
         verbatim excerpt copied from the current file: no paraphrasing, ellipses, line-number prefixes, \
         or Markdown reformatting. The line range must contain the quote; if exact evidence is unavailable, \
         answer unknown. Checks: {}",
        checks
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_file_evidence_and_reject_stale_quotes() {
        let root = std::env::temp_dir().join(format!(
            "aitui-report-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&root).expect("create fixture directory");
        std::fs::write(root.join("evidence.txt"), "first\nverified fact\nthird\n")
            .expect("write fixture");
        let checks = vec![CheckSpec {
            id: "fact".into(),
            question: "is the fact present?".into(),
        }];
        let report = |quote: &str| {
            format!(
                r#"{{"schema":"aitui.child-report.v1","status":"complete","findings":[{{"check_id":"fact","answer":"yes","statement":"checked","evidence":[{{"kind":"file","path":"evidence.txt","line_start":2,"line_end":2,"quote":"{}"}}]}}],"uncertainties":[]}}"#,
                quote
            )
        };
        assert!(parse_and_validate(&report("verified fact"), &checks, &root).is_ok());
        let (stale, warnings) =
            parse_and_validate_detailed(&report("stale claim"), &checks, &root).unwrap();
        assert!(stale.findings.is_empty());
        assert!(warnings.iter().any(|warning| warning.contains("stale")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn relocates_unique_quotes_and_normalizes_wrapped_whitespace() {
        let root = std::env::temp_dir().join(format!(
            "aitui-report-relocate-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("evidence.md"),
            "heading\nnew prefix\nThe ownership contract is\nexplicit and testable.\n",
        )
        .unwrap();
        let checks = vec![CheckSpec {
            id: "contract".into(),
            question: "is the contract explicit?".into(),
        }];
        let text = r#"{"schema":"aitui.child-report.v1","status":"complete","findings":[{"check_id":"contract","answer":"yes","statement":"checked","evidence":[{"kind":"file","path":"evidence.md","line_start":1,"line_end":1,"quote":"The ownership contract is explicit and testable."}]}],"uncertainties":[]}"#;
        let report = parse_and_validate(text, &checks, &root).unwrap();
        let EvidenceRef::File {
            line_start,
            line_end,
            ..
        } = &report.findings[0].evidence[0]
        else {
            panic!("expected file evidence");
        };
        assert_eq!((*line_start, *line_end), (3, 4));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_evidence_does_not_discard_other_findings() {
        let root = std::env::temp_dir().join(format!(
            "aitui-report-partial-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("evidence.txt"),
            "a sufficiently specific valid fact\n",
        )
        .unwrap();
        let checks = vec![
            CheckSpec {
                id: "valid".into(),
                question: "valid?".into(),
            },
            CheckSpec {
                id: "stale".into(),
                question: "stale?".into(),
            },
        ];
        let text = r#"{"schema":"aitui.child-report.v1","status":"complete","findings":[{"check_id":"valid","answer":"yes","statement":"valid","evidence":[{"kind":"file","path":"evidence.txt","line_start":1,"line_end":1,"quote":"a sufficiently specific valid fact"}]},{"check_id":"stale","answer":"no","statement":"stale","evidence":[{"kind":"file","path":"evidence.txt","line_start":1,"line_end":1,"quote":"a fabricated stale quotation"}]}],"uncertainties":[]}"#;
        let (report, warnings) = parse_and_validate_detailed(text, &checks, &root).unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].check_id, "valid");
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("check 'stale'")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn disagreement_remains_explicit() {
        let checks = vec![CheckSpec {
            id: "x".into(),
            question: "is x true?".into(),
        }];
        let report = |answer| ChildReport {
            schema: "aitui.child-report.v1".into(),
            status: ReportStatus::Complete,
            findings: vec![Finding {
                check_id: "x".into(),
                answer,
                statement: "checked".into(),
                evidence: vec![EvidenceRef::Command {
                    command: "cargo test".into(),
                    exit_code: 0,
                    output_excerpt: "ok".into(),
                }],
            }],
            uncertainties: Vec::new(),
        };
        let summary = reconcile(
            &[report(FindingAnswer::Yes), report(FindingAnswer::No)],
            &checks,
        );
        assert_eq!(summary.status, "unresolved");
        assert_eq!(summary.unresolved, vec!["x"]);
    }

    #[test]
    fn matching_mixed_and_unknown_answers_are_consensus() {
        let checks = vec![CheckSpec {
            id: "x".into(),
            question: "is x true?".into(),
        }];
        let report = |answer| ChildReport {
            schema: "aitui.child-report.v1".into(),
            status: ReportStatus::Complete,
            findings: vec![Finding {
                check_id: "x".into(),
                answer,
                statement: "checked".into(),
                evidence: vec![EvidenceRef::Command {
                    command: "cargo test".into(),
                    exit_code: 0,
                    output_excerpt: "ok".into(),
                }],
            }],
            uncertainties: Vec::new(),
        };
        for answer in [FindingAnswer::Mixed, FindingAnswer::Unknown] {
            let summary = reconcile(&[report(answer.clone()), report(answer.clone())], &checks);
            assert_eq!(summary.status, "verified");
            assert_eq!(summary.findings[0].answer, answer);
        }
    }

    #[test]
    fn finds_embedded_verification_summaries_and_preserves_boundaries() {
        let text = concat!(
            "agent 1 (completed):\n",
            "{\"schema\":\"aitui.verification-summary.v1\",\"status\":\"verified\",",
            "\"findings\":[{\"check_id\":\"latency\",\"answer\":\"yes\",",
            "\"statement\":\"Budgets are generous.\",\"support\":\"2/2 replicas\",",
            "\"evidence\":[\"src/agent/subtask.rs:1-2\"]}],\"unresolved\":[],",
            "\"diagnostics\":[]}\n\n---\n\nagent 2 (unresolved):\n",
            "{\"schema\":\"aitui.verification-summary.v1\",\"status\":\"unresolved\",",
            "\"findings\":[],\"unresolved\":[\"access\"],\"diagnostics\":[\"invalid report\"]}"
        );
        let reports = verification_reports(text);
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].report.status, "verified");
        assert_eq!(reports[0].report.findings[0].answer, FindingAnswer::Yes);
        assert_eq!(reports[1].report.status, "unresolved");
        assert_eq!(reports[1].report.unresolved, vec!["access"]);
        let extracted: serde_json::Value =
            serde_json::from_str(&text[reports[0].start..reports[0].end]).unwrap();
        assert_eq!(
            extracted,
            serde_json::json!({
                "schema": "aitui.verification-summary.v1",
                "status": "verified",
                "findings": [{
                    "check_id": "latency",
                    "answer": "yes",
                    "statement": "Budgets are generous.",
                    "support": "2/2 replicas",
                    "evidence": ["src/agent/subtask.rs:1-2"]
                }],
                "unresolved": [],
                "diagnostics": []
            })
        );
    }

    #[test]
    fn ignores_unrelated_json_objects() {
        assert!(verification_reports("before {\"status\":\"verified\"} after").is_empty());
        assert!(verification_report("{\"schema\":\"other\",\"status\":\"verified\"}").is_none());
    }

    #[test]
    fn reconcile_requires_two_supported_matching_answers() {
        let checks = vec![CheckSpec {
            id: "x".into(),
            question: "is x true?".into(),
        }];
        let report = |answer| ChildReport {
            schema: "aitui.child-report.v1".into(),
            status: ReportStatus::Complete,
            findings: vec![Finding {
                check_id: "x".into(),
                answer,
                statement: "checked".into(),
                evidence: vec![EvidenceRef::Command {
                    command: "cargo test".into(),
                    exit_code: 0,
                    output_excerpt: "ok".into(),
                }],
            }],
            uncertainties: Vec::new(),
        };
        assert_eq!(
            reconcile(&[report(FindingAnswer::Yes)], &checks).status,
            "unresolved"
        );
        assert_eq!(
            reconcile(
                &[report(FindingAnswer::Yes), report(FindingAnswer::Yes)],
                &checks
            )
            .status,
            "verified"
        );
    }
}
