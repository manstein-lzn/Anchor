//! Automatic reclamation of settled Runs (design card 003).
//!
//! A settled Run is reclaimed when it settled more than `max_age` ago, or,
//! oldest first, while the Runs' data together exceed `max_bytes`. Every
//! reclamation goes through the one Run deletion path, so a Run that path
//! refuses (still referenced, the current turn of a conversation, an
//! undelivered reply) is skipped and counted, never forced. Hub facts and
//! grades are not Run data and stay.

use std::path::Path;
use std::time::Duration;

/// Settled Runs older than this are reclaimed unless configured otherwise.
const DEFAULT_MAX_AGE_DAYS: u64 = 30;
/// Runs' data beyond this total is reclaimed, oldest first.
const DEFAULT_MAX_BYTES: u64 = 20 * 1024 * 1024 * 1024;
/// How often the serve process reclaims after start-up.
pub(crate) const INTERVAL: Duration = Duration::from_secs(3600);
/// Hub facts and grades beyond this total are reported, never deleted.
const FACTS_ALARM_BYTES: u64 = 5 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Policy {
    /// `None` disables reclamation by age.
    pub(crate) max_age: Option<Duration>,
    /// `None` disables reclamation by size.
    pub(crate) max_bytes: Option<u64>,
}

impl Policy {
    /// From `ANCHOR_RETENTION_DAYS` and `ANCHOR_RETENTION_MAX_GB`: unset means
    /// the default, `0` or `off` disables that limit.
    pub(crate) fn from_env() -> Result<Self, String> {
        fn limit(name: &str, default: u64) -> Result<Option<u64>, String> {
            match std::env::var(name) {
                Err(_) => Ok(Some(default)),
                Ok(value) if value == "off" || value == "0" => Ok(None),
                Ok(value) => value
                    .parse::<u64>()
                    .map(Some)
                    .map_err(|_| format!("{name} must be a whole number, 0 or off")),
            }
        }
        Ok(Self {
            max_age: limit("ANCHOR_RETENTION_DAYS", DEFAULT_MAX_AGE_DAYS)?
                .map(|days| Duration::from_secs(days * 86_400)),
            max_bytes: limit("ANCHOR_RETENTION_MAX_GB", DEFAULT_MAX_BYTES >> 30)?
                .map(|gb| gb << 30),
        })
    }

    pub(crate) fn disabled(&self) -> bool {
        self.max_age.is_none() && self.max_bytes.is_none()
    }
}

/// A settled Run as reclamation sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub(crate) run_id: String,
    /// Seconds since the Unix epoch at which it last changed, which for a
    /// settled Run is when it settled.
    pub(crate) settled_at: i64,
    pub(crate) bytes: u64,
}

/// The settled Runs to reclaim, oldest first: every one past the age limit,
/// then further ones, oldest first, until the rest fit under the size limit.
/// `total` is the data of every Run, settled or not, so a busy deployment
/// still reclaims its settled history to make room.
pub(crate) fn select(
    policy: &Policy,
    mut settled: Vec<Candidate>,
    total: u64,
    now: i64,
) -> Vec<Candidate> {
    settled.sort_by(|a, b| (a.settled_at, &a.run_id).cmp(&(b.settled_at, &b.run_id)));
    let mut remaining = total;
    let mut chosen = Vec::new();
    for candidate in settled {
        let expired = policy
            .max_age
            .is_some_and(|age| now.saturating_sub(candidate.settled_at) > age.as_secs() as i64);
        let over = policy.max_bytes.is_some_and(|limit| remaining > limit);
        if expired || over {
            remaining = remaining.saturating_sub(candidate.bytes);
            chosen.push(candidate);
        }
    }
    chosen
}

/// What one pass did, for the one log line it writes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Report {
    pub(crate) removed: usize,
    pub(crate) expired_artifacts: usize,
    pub(crate) freed_bytes: u64,
    /// Runs selected but refused by the deletion path, with the reason.
    pub(crate) skipped: Vec<(String, String)>,
    pub(crate) remaining_bytes: u64,
    pub(crate) facts_bytes: u64,
}

impl Report {
    pub(crate) fn line(&self) -> Option<String> {
        let alarm = self.facts_bytes > FACTS_ALARM_BYTES;
        if self.removed == 0 && self.expired_artifacts == 0 && self.skipped.is_empty() && !alarm {
            return None;
        }
        let mut line = format!(
            "anchor-runner-host: retention: removed {} settled Run(s), freed {} MB, {} MB of Run data remain",
            self.removed,
            self.freed_bytes >> 20,
            self.remaining_bytes >> 20
        );
        if self.expired_artifacts > 0 {
            line.push_str(&format!(
                "; expired {} delivery version(s)",
                self.expired_artifacts
            ));
        }
        if !self.skipped.is_empty() {
            let reasons = self
                .skipped
                .iter()
                .take(5)
                .map(|(run, reason)| format!("{run}: {reason}"))
                .collect::<Vec<_>>()
                .join("; ");
            line.push_str(&format!("; skipped {} ({reasons})", self.skipped.len()));
        }
        if alarm {
            line.push_str(&format!(
                "; hub facts and grades hold {} MB, above the {} MB alarm; they are kept",
                self.facts_bytes >> 20,
                FACTS_ALARM_BYTES >> 20
            ));
        }
        Some(line)
    }
}

/// Bytes under `path`, not following symlinks; 0 when it is absent.
pub(crate) fn size(path: &Path) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !metadata.is_dir() {
        return metadata.len();
    }
    std::fs::read_dir(path)
        .map(|entries| entries.flatten().map(|entry| size(&entry.path())).sum())
        .unwrap_or(0)
}

/// Hub facts and grades: kept, only measured. The roots are named by their
/// kinds so this measurement does not read as a grade writer to the
/// anchor-eval fence, which guards who opens the grade root.
pub(crate) fn facts_bytes(data_root: &Path) -> u64 {
    ["facts", "grades"]
        .iter()
        .map(|kind| size(&data_root.join(format!("kernel-{kind}"))))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str, settled_at: i64, bytes: u64) -> Candidate {
        Candidate {
            run_id: id.into(),
            settled_at,
            bytes,
        }
    }

    const DAY: i64 = 86_400;

    #[test]
    fn age_reclaims_only_runs_past_it() {
        let policy = Policy {
            max_age: Some(Duration::from_secs(30 * DAY as u64)),
            max_bytes: None,
        };
        let now = 100 * DAY;
        let chosen = select(
            &policy,
            vec![run("new", now - DAY, 5), run("old", now - 31 * DAY, 5)],
            10,
            now,
        );
        assert_eq!(chosen, vec![run("old", now - 31 * DAY, 5)]);
    }

    #[test]
    fn size_reclaims_oldest_first_until_the_rest_fit() {
        let policy = Policy {
            max_age: None,
            max_bytes: Some(10),
        };
        // 4 + 4 + 4 settled, plus 3 of an unsettled Run: 15 in all.
        let chosen = select(
            &policy,
            vec![run("c", 3, 4), run("a", 1, 4), run("b", 2, 4)],
            15,
            100,
        );
        assert_eq!(
            chosen.iter().map(|c| c.run_id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    #[test]
    fn either_limit_is_enough_and_both_off_reclaims_nothing() {
        let both = Policy {
            max_age: Some(Duration::from_secs(10)),
            max_bytes: Some(100),
        };
        let chosen = select(&both, vec![run("old", 0, 1), run("new", 95, 1)], 2, 100);
        assert_eq!(chosen.len(), 1);
        let off = Policy {
            max_age: None,
            max_bytes: None,
        };
        assert!(off.disabled());
        assert!(select(&off, vec![run("old", 0, 1_000)], 1_000_000, 1_000_000).is_empty());
    }

    #[test]
    fn the_environment_sets_or_disables_each_limit() {
        let _env = crate::PROCESS_ENV_LOCK.blocking_lock();
        let restore = [
            (
                "ANCHOR_RETENTION_DAYS",
                std::env::var_os("ANCHOR_RETENTION_DAYS"),
            ),
            (
                "ANCHOR_RETENTION_MAX_GB",
                std::env::var_os("ANCHOR_RETENTION_MAX_GB"),
            ),
        ];
        unsafe {
            std::env::remove_var("ANCHOR_RETENTION_DAYS");
            std::env::remove_var("ANCHOR_RETENTION_MAX_GB");
        }
        let default = Policy::from_env().unwrap();
        assert_eq!(default.max_age, Some(Duration::from_secs(30 * 86_400)));
        assert_eq!(default.max_bytes, Some(20 << 30));
        unsafe {
            std::env::set_var("ANCHOR_RETENTION_DAYS", "off");
            std::env::set_var("ANCHOR_RETENTION_MAX_GB", "5");
        }
        let custom = Policy::from_env().unwrap();
        assert_eq!(custom.max_age, None);
        assert_eq!(custom.max_bytes, Some(5 << 30));
        unsafe {
            std::env::set_var("ANCHOR_RETENTION_MAX_GB", "lots");
        }
        assert!(Policy::from_env().is_err());
        for (name, value) in restore {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[test]
    fn the_report_is_quiet_when_nothing_happened_and_names_skips_and_the_alarm() {
        assert_eq!(Report::default().line(), None);
        let report = Report {
            removed: 2,
            expired_artifacts: 0,
            freed_bytes: 3 << 20,
            skipped: vec![("r1".into(), "still referenced".into())],
            remaining_bytes: 7 << 20,
            facts_bytes: 6 << 30,
        };
        let line = report.line().unwrap();
        assert!(
            line.contains("removed 2 settled Run(s), freed 3 MB, 7 MB"),
            "{line}"
        );
        assert!(line.contains("skipped 1 (r1: still referenced)"), "{line}");
        assert!(
            line.contains("above the 5120 MB alarm; they are kept"),
            "{line}"
        );
    }
}
