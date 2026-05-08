use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// A concise GitHub Actions history sample for a workflow or job. The learner
/// uses these as timing hints so slow remote suites do not become local fast
/// checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowHistoryHint {
    pub origin: String,
    pub conclusion: String,
    pub duration_seconds: u64,
    pub sample_count: usize,
    pub url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhWorkflowRun {
    database_id: u64,
    workflow_name: Option<String>,
    display_title: Option<String>,
    status: Option<String>,
    conclusion: Option<String>,
    created_at: Option<String>,
    started_at: Option<String>,
    updated_at: Option<String>,
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhRunJobs {
    jobs: Vec<GhWorkflowJob>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhWorkflowJob {
    name: Option<String>,
    status: Option<String>,
    conclusion: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
}

pub(crate) fn collect_workflow_history_hints(repo_root: &Path) -> Vec<WorkflowHistoryHint> {
    let Some(repo) = github_repo_slug(repo_root) else {
        return Vec::new();
    };

    let args = vec![
        "run".to_string(),
        "list".to_string(),
        "--repo".to_string(),
        repo.clone(),
        "--limit".to_string(),
        "20".to_string(),
        "--json".to_string(),
        "databaseId,workflowName,displayTitle,status,conclusion,createdAt,startedAt,updatedAt,url"
            .to_string(),
    ];
    let Some(output) = gh_stdout(repo_root, &args) else {
        return Vec::new();
    };
    let Ok(runs) = serde_json::from_slice::<Vec<GhWorkflowRun>>(&output) else {
        return Vec::new();
    };

    let mut hints_by_origin = BTreeMap::new();
    for run in runs
        .iter()
        .filter(|run| is_completed(run.status.as_deref()))
    {
        if let Some(duration_seconds) = duration_between(
            run.started_at.as_deref().or(run.created_at.as_deref()),
            run.updated_at.as_deref(),
        ) {
            upsert_history_hint(
                &mut hints_by_origin,
                WorkflowHistoryHint {
                    origin: run_origin(run),
                    conclusion: run
                        .conclusion
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string()),
                    duration_seconds,
                    sample_count: 1,
                    url: run.url.clone(),
                },
            );
        }
    }

    for run in runs
        .iter()
        .filter(|run| is_completed(run.status.as_deref()))
        .take(8)
    {
        let run_id = run.database_id.to_string();
        let args = vec![
            "run".to_string(),
            "view".to_string(),
            run_id,
            "--repo".to_string(),
            repo.clone(),
            "--json".to_string(),
            "jobs".to_string(),
        ];
        let Some(output) = gh_stdout(repo_root, &args) else {
            continue;
        };
        let Ok(run_jobs) = serde_json::from_slice::<GhRunJobs>(&output) else {
            continue;
        };
        for job in run_jobs
            .jobs
            .into_iter()
            .filter(|job| is_completed(job.status.as_deref()))
        {
            let Some(duration_seconds) =
                duration_between(job.started_at.as_deref(), job.completed_at.as_deref())
            else {
                continue;
            };
            upsert_history_hint(
                &mut hints_by_origin,
                WorkflowHistoryHint {
                    origin: format!("{}::{}", run_origin(run), job.name.unwrap_or_default()),
                    conclusion: job.conclusion.unwrap_or_else(|| "unknown".to_string()),
                    duration_seconds,
                    sample_count: 1,
                    url: run.url.clone(),
                },
            );
        }
    }

    let mut hints = hints_by_origin.into_values().collect::<Vec<_>>();
    hints.sort_by(|left, right| {
        right
            .duration_seconds
            .cmp(&left.duration_seconds)
            .then_with(|| left.origin.cmp(&right.origin))
    });
    hints.truncate(30);
    hints
}

fn github_repo_slug(repo_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    normalize_github_remote(String::from_utf8_lossy(&output.stdout).trim())
}

fn normalize_github_remote(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/').trim_end_matches(".git");
    if let Some(rest) = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("git://github.com/"))
    {
        return Some(rest.trim_matches('/').to_string());
    }
    url.strip_prefix("git@github.com:")
        .map(|rest| rest.trim_matches('/').to_string())
}

fn gh_stdout(repo_root: &Path, args: &[String]) -> Option<Vec<u8>> {
    let output = Command::new("gh")
        .args(args)
        .current_dir(repo_root)
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn is_completed(status: Option<&str>) -> bool {
    status.is_none_or(|status| status.eq_ignore_ascii_case("completed"))
}

fn run_origin(run: &GhWorkflowRun) -> String {
    run.workflow_name
        .clone()
        .or_else(|| run.display_title.clone())
        .unwrap_or_else(|| format!("run {}", run.database_id))
}

fn upsert_history_hint(
    hints_by_origin: &mut BTreeMap<String, WorkflowHistoryHint>,
    hint: WorkflowHistoryHint,
) {
    hints_by_origin
        .entry(hint.origin.clone())
        .and_modify(|existing| {
            existing.sample_count += 1;
            if hint.duration_seconds > existing.duration_seconds {
                existing.duration_seconds = hint.duration_seconds;
                existing.conclusion.clone_from(&hint.conclusion);
                existing.url.clone_from(&hint.url);
            }
        })
        .or_insert(hint);
}

fn duration_between(start: Option<&str>, end: Option<&str>) -> Option<u64> {
    let start = parse_github_timestamp_to_unix_seconds(start?)?;
    let end = parse_github_timestamp_to_unix_seconds(end?)?;
    (end >= start).then_some((end - start) as u64)
}

fn parse_github_timestamp_to_unix_seconds(timestamp: &str) -> Option<i64> {
    let timestamp = timestamp.trim();
    let timestamp = timestamp.strip_suffix('Z').unwrap_or(timestamp);
    let (date, time) = timestamp.split_once('T')?;
    let mut date_parts = date.split('-');
    let year = date_parts.next()?.parse::<i32>().ok()?;
    let month = date_parts.next()?.parse::<u32>().ok()?;
    let day = date_parts.next()?.parse::<u32>().ok()?;
    if date_parts.next().is_some() {
        return None;
    }

    let time = time.split_once('.').map_or(time, |(whole, _)| whole);
    let mut time_parts = time.split(':');
    let hour = time_parts.next()?.parse::<u32>().ok()?;
    let minute = time_parts.next()?.parse::<u32>().ok()?;
    let second = time_parts.next()?.parse::<u32>().ok()?;
    if time_parts.next().is_some()
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }

    Some(
        days_from_civil(year, month, day) * 86_400
            + i64::from(hour) * 3_600
            + i64::from(minute) * 60
            + i64::from(second),
    )
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = year - i32::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = month as i32;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day as i32 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    i64::from(era) * 146_097 + i64::from(day_of_era) - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn duration_between_github_timestamps_handles_utc_seconds() {
        assert_eq!(
            duration_between(Some("2026-04-30T21:19:14Z"), Some("2026-04-30T21:37:08Z")),
            Some(1_074)
        );
    }

    #[test]
    fn normalize_github_remote_extracts_owner_repo() {
        assert_eq!(
            normalize_github_remote("git@github.com:scylladb/python-driver.git").as_deref(),
            Some("scylladb/python-driver")
        );
        assert_eq!(
            normalize_github_remote("https://github.com/scylladb/python-driver").as_deref(),
            Some("scylladb/python-driver")
        );
    }
}
