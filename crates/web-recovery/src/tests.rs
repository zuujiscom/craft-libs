use crate::plan::{Listed, Plan, plan};
use crate::{Warning, now_ms};

fn entry(name: &str, session: &str, saved_at: f64, size: Option<f64>) -> Listed {
    Listed { name: name.into(), session: session.into(), saved_at, size }
}

#[test]
fn running_sessions_and_our_own_are_left_alone() {
    let listed = [entry("mine", "me", 1.0, Some(1.0)), entry("open tab", "other", 2.0, Some(1.0)), entry("crashed", "gone", 3.0, Some(1.0))];
    let p = plan(&listed, "me", Some(&["other".to_string()]), 100.0);
    assert_eq!(p, Plan { load: vec![2], damaged: vec![], warnings: vec![] });
}

#[test]
fn without_web_locks_only_our_own_entries_are_skipped() {
    let listed = [entry("mine", "me", 1.0, Some(1.0)), entry("other", "other", 2.0, Some(1.0))];
    assert_eq!(plan(&listed, "me", None, 100.0).load, vec![1]);
}

#[test]
fn newest_first_within_the_byte_budget() {
    let listed = [entry("old", "a", 1.0, Some(60.0)), entry("new", "b", 3.0, Some(60.0)), entry("mid", "c", 2.0, Some(30.0))];
    let p = plan(&listed, "me", Some(&[]), 100.0);
    assert_eq!(p.load, vec![1, 2]);
    assert_eq!(p.warnings, vec![Warning::TooLarge { name: "old".into() }]);
}

#[test]
fn damaged_copies_are_reported_and_deleted_but_not_a_running_tabs() {
    let listed = [entry("broken", "gone", 1.0, None), entry("busy", "other", 1.0, None)];
    let p = plan(&listed, "me", Some(&["other".to_string()]), 100.0);
    assert_eq!(p.damaged, vec![0]);
    assert!(p.load.is_empty());
    assert_eq!(p.warnings, vec![Warning::Damaged { name: "broken".into() }]);
}

#[test]
fn warnings_read_as_sentences() {
    assert_eq!(
        Warning::TooLarge { name: "Poster".into() }.to_string(),
        "Poster was not recovered: the recovered documents are larger than this page reads at once"
    );
}

#[test]
fn the_clock_runs() {
    assert!(now_ms() > 1.0e12);
}
