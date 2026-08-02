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

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
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
}

pub fn parse_and_validate(
    text: &str,
    checks: &[CheckSpec],
    cwd: &Path,
) -> Result<ChildReport, String> {
    if text.len() > MAX_REPORT_BYTES {
        return Err("report exceeds size limit".into());
    }
    let start = text
        .find('{')
        .ok_or_else(|| "report does not contain a JSON object".to_string())?;
    let report: ChildReport = serde_json::from_str(&text[start..])
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
        if !matches!(finding.answer, FindingAnswer::Unknown) && finding.evidence.is_empty() {
            return Err(format!("finding '{}' has no evidence", finding.check_id));
        }
        for evidence in &finding.evidence {
            validate_evidence(evidence, cwd)?;
        }
    }
    Ok(report)
}

fn validate_evidence(evidence: &EvidenceRef, cwd: &Path) -> Result<(), String> {
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
        PathBuf::from(path)
    } else {
        cwd.join(path)
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
    if *line_end > lines.len() {
        return Err(format!("evidence line range exceeds '{}'", path));
    }
    let excerpt = lines[*line_start - 1..*line_end].join("\n");
    if !excerpt.contains(quote.trim()) {
        return Err(format!("evidence quote is stale for '{}'", path));
    }
    Ok(())
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
        let accepted = [("yes", FindingAnswer::Yes), ("no", FindingAnswer::No)]
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
         \"uncertainties\":[]}}. Every non-unknown answer needs evidence. Checks: {}",
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
        assert!(parse_and_validate(&report("stale claim"), &checks, &root).is_err());
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
