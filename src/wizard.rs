//! Picks what to free for a size target: safe items biggest first until the target is met,
//! "check first" items only when the safe ones fall short. No UI, no I/O.

use crate::rules::{Finding, Safety};
use std::cmp::Reverse;
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    Bytes(u64),
    AllSafe,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Source {
    Finding(PathBuf),
    /// Id as used by `request_system_batch`, e.g. "homebrew".
    System(String),
    /// A copy the duplicate finder would remove.
    Duplicate(PathBuf),
    /// An unused app (with its leftovers).
    App(PathBuf),
    /// An old screenshot, installer or download.
    Old(PathBuf),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub source: Source,
    pub size: u64,
}

#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// Ticked at start, biggest first.
    pub ticked: Vec<Candidate>,
    /// Safe items the target does not need, unticked.
    pub more_safe: Vec<Candidate>,
    /// "Check first" items, shown only when all safe ones fall short of the target.
    pub review: Vec<Candidate>,
    pub safe_total: u64,
}

pub fn plan(findings: &[Finding], system: &[(String, u64)], dupes: &[(PathBuf, u64)], apps: &[(PathBuf, u64)], old: &[(PathBuf, u64)], target: Target) -> Plan {
    let of = |safety: Safety| findings.iter().filter(move |f| f.safety == safety && f.size > 0).map(|f| Candidate { source: Source::Finding(f.path.clone()), size: f.size });
    let mut safe: Vec<Candidate> = of(Safety::Safe).chain(system.iter().filter(|(_, n)| *n > 0).map(|(id, n)| Candidate { source: Source::System(id.clone()), size: *n })).collect();
    safe.sort_by_key(|c| Reverse(c.size));
    let safe_total = safe.iter().map(|c| c.size).sum();
    let Target::Bytes(goal) = target else { return Plan { ticked: safe, safe_total, ..Plan::default() } };
    let (mut n, mut sum) = (0, 0);
    while n < safe.len() && sum < goal {
        sum += safe[n].size;
        n += 1;
    }
    let more_safe = safe.split_off(n);
    // An old file that is, or sits inside, a finding is already offered through that finding.
    let covered = |s: &Source| matches!(s, Source::Old(p) if findings.iter().any(|f| f.safety != Safety::Keep && p.starts_with(&f.path)));
    let paths = |list: &[(PathBuf, u64)], source: fn(PathBuf) -> Source| list.iter().filter(|(_, n)| *n > 0).map(move |(p, n)| Candidate { source: source(p.clone()), size: *n }).collect::<Vec<_>>();
    let mut review: Vec<Candidate> = if safe_total < goal { of(Safety::Review).chain(paths(dupes, Source::Duplicate)).chain(paths(apps, Source::App)).chain(paths(old, Source::Old).into_iter().filter(|c| !covered(&c.source))).collect() } else { Vec::new() };
    review.sort_by_key(|c| Reverse(c.size));
    Plan { ticked: safe, more_safe, review, safe_total }
}

/// Ticks for a rebuilt plan: the plan's own picks when `fresh` (new target, or the user has not
/// touched a box yet), else the user's ticks minus items that are no longer in the plan.
pub fn reconcile(ticks: &HashSet<Source>, plan: &Plan, fresh: bool) -> HashSet<Source> {
    if fresh {
        return plan.ticked.iter().map(|c| c.source.clone()).collect();
    }
    plan.ticked.iter().chain(&plan.more_safe).chain(&plan.review).map(|c| &c.source).filter(|s| ticks.contains(s)).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(path: &str, size: u64, safety: Safety) -> Finding {
        Finding { path: PathBuf::from(path), size, rule_id: "r".into(), safety, message: String::new() }
    }

    fn sizes(c: &[Candidate]) -> Vec<u64> {
        c.iter().map(|c| c.size).collect()
    }

    #[test]
    fn ticks_biggest_safe_first_until_target() {
        let fs = [f("/a", 3, Safety::Safe), f("/b", 8, Safety::Safe), f("/c", 5, Safety::Safe)];
        let p = plan(&fs, &[("homebrew".into(), 1)], &[], &[], &[], Target::Bytes(10));
        assert_eq!(sizes(&p.ticked), [8, 5]);
        assert_eq!(sizes(&p.more_safe), [3, 1]);
        assert!(p.review.is_empty());
        assert_eq!(p.safe_total, 17);
    }

    #[test]
    fn all_safe_ticks_everything_safe_and_no_review() {
        let fs = [f("/a", 3, Safety::Safe), f("/r", 50, Safety::Review)];
        let p = plan(&fs, &[("homebrew".into(), 4)], &[], &[], &[], Target::AllSafe);
        assert_eq!(sizes(&p.ticked), [4, 3]);
        assert_eq!(p.ticked[0].source, Source::System("homebrew".into()));
        assert!(p.more_safe.is_empty() && p.review.is_empty());
    }

    #[test]
    fn review_only_when_safe_falls_short() {
        let fs = [f("/a", 3, Safety::Safe), f("/r1", 5, Safety::Review), f("/r2", 9, Safety::Review)];
        assert!(plan(&fs, &[], &[], &[], &[], Target::Bytes(3)).review.is_empty());
        let p = plan(&fs, &[], &[], &[], &[], Target::Bytes(100));
        assert_eq!(sizes(&p.ticked), [3]);
        assert_eq!(sizes(&p.review), [9, 5]);
    }

    #[test]
    fn keep_and_zero_sizes_never_appear() {
        let fs = [f("/k", 99, Safety::Keep), f("/z", 0, Safety::Safe), f("/zr", 0, Safety::Review)];
        let p = plan(&fs, &[("docker".into(), 0)], &[], &[], &[], Target::Bytes(1000));
        assert_eq!(p, Plan::default());
    }

    #[test]
    fn empty_input_gives_empty_plan() {
        assert_eq!(plan(&[], &[], &[], &[], &[], Target::Bytes(10)), Plan::default());
        assert_eq!(plan(&[], &[], &[], &[], &[], Target::AllSafe), Plan::default());
    }

    #[test]
    fn reconcile_fresh_takes_the_plans_ticks() {
        let fs = [f("/a", 8, Safety::Safe), f("/b", 5, Safety::Safe)];
        let p = plan(&fs, &[], &[], &[], &[], Target::Bytes(6));
        let old: HashSet<Source> = [Source::Finding("/b".into())].into();
        assert_eq!(reconcile(&old, &p, true), [Source::Finding("/a".into())].into());
    }

    #[test]
    fn reconcile_keeps_user_ticks_but_drops_vanished_items() {
        let fs = [f("/a", 8, Safety::Safe), f("/b", 5, Safety::Safe)];
        let p = plan(&fs, &[], &[], &[], &[], Target::Bytes(6));
        let mine: HashSet<Source> = [Source::Finding("/b".into()), Source::Finding("/gone".into())].into();
        assert_eq!(reconcile(&mine, &p, false), [Source::Finding("/b".into())].into());
    }

    #[test]
    fn duplicates_are_review_only() {
        let fs = [f("/a", 3, Safety::Safe)];
        let p = plan(&fs, &[], &[(PathBuf::from("/d"), 50)], &[], &[], Target::Bytes(100));
        assert_eq!(sizes(&p.ticked), [3]);
        assert_eq!(p.review, [Candidate { source: Source::Duplicate("/d".into()), size: 50 }]);
        assert!(plan(&fs, &[], &[(PathBuf::from("/d"), 50)], &[], &[], Target::Bytes(2)).review.is_empty());
    }

    #[test]
    fn plan_follows_changed_findings() {
        let before = [f("/a", 8, Safety::Safe), f("/b", 5, Safety::Safe)];
        let after = [f("/b", 5, Safety::Safe)];
        assert_eq!(plan(&before, &[], &[], &[], &[], Target::Bytes(6)).ticked[0].source, Source::Finding("/a".into()));
        assert_eq!(plan(&after, &[], &[], &[], &[], Target::Bytes(6)).ticked[0].source, Source::Finding("/b".into()));
    }

    #[test]
    fn unused_apps_are_only_suggested_for_review() {
        let fs = [f("/a", 5, Safety::Safe)];
        let apps = [(PathBuf::from("/Applications/Old.app"), 50)];
        let p = plan(&fs, &[], &[], &apps, &[], Target::Bytes(100));
        assert_eq!(p.review, vec![Candidate { source: Source::App(PathBuf::from("/Applications/Old.app")), size: 50 }]);
        assert!(p.ticked.iter().chain(&p.more_safe).all(|c| !matches!(c.source, Source::App(_))));
        assert!(plan(&fs, &[], &[], &apps, &[], Target::Bytes(5)).review.is_empty(), "safe items suffice");
    }

    #[test]
    fn old_files_are_only_suggested_for_review() {
        let fs = [f("/a", 5, Safety::Safe)];
        let old = [(PathBuf::from("/d/old.zip"), 50)];
        let p = plan(&fs, &[], &[], &[], &old, Target::Bytes(100));
        assert_eq!(p.review, vec![Candidate { source: Source::Old(PathBuf::from("/d/old.zip")), size: 50 }]);
        assert!(plan(&fs, &[], &[], &[], &old, Target::Bytes(5)).review.is_empty(), "safe items suffice");
    }

    #[test]
    fn an_old_file_that_is_also_a_finding_is_offered_once() {
        let fs = [f("/d/Zoom.pkg", 90, Safety::Safe), f("/dl", 500, Safety::Review)];
        let old = [(PathBuf::from("/d/Zoom.pkg"), 90), (PathBuf::from("/dl/report.pdf"), 40), (PathBuf::from("/x/shot.png"), 5)];
        let p = plan(&fs, &[], &[], &[], &old, Target::Bytes(10_000));
        let olds: Vec<&Source> = p.review.iter().map(|c| &c.source).filter(|s| matches!(s, Source::Old(_))).collect();
        assert_eq!(olds, vec![&Source::Old(PathBuf::from("/x/shot.png"))], "the installer and the file inside a found folder are already offered");
    }
}
