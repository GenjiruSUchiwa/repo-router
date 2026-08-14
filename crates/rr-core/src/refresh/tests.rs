//! Plan, identity, and rendering rules — everything a refresh decides before
//! it touches a repository.

use super::*;
use crate::lang::Lang;

#[track_caller]
fn path(value: &str) -> RelPath {
    match RelPath::new(value) {
        Ok(path) => path,
        Err(error) => panic!("test path {value:?} is invalid: {error}"),
    }
}

fn paths(values: &[RelPath]) -> Vec<&str> {
    values.iter().map(RelPath::as_str).collect()
}

#[track_caller]
fn built(draft: PlanDraft) -> RefreshPlan {
    match draft.build() {
        Ok(plan) => plan,
        Err(error) => panic!("expected a valid plan, got {error}"),
    }
}

// --- dispositions -----------------------------------------------------------

#[test]
fn an_empty_draft_is_the_no_op_precondition() {
    let plan = built(PlanDraft::new());

    assert!(plan.is_empty_delta());
    assert_eq!(plan.mode(), RefreshMode::Incremental);
    assert_eq!(plan.reason(), None);
}

#[test]
fn a_full_plan_is_never_an_empty_delta_however_quiet_the_repository_is() {
    let plan = RefreshPlan::full(FullReason::HeadChanged);

    assert!(!plan.is_empty_delta(), "a full plan must not no-op");
    assert!(!plan.may_retain(&path("src/lib.rs")));
    assert_eq!(plan.reason(), Some(FullReason::HeadChanged));
}

#[test]
fn additions_modifications_and_type_changes_all_recheck_the_current_entry() {
    let mut draft = PlanDraft::new();
    draft.recheck(path("src/added.rs"));
    draft.recheck(path("src/modified.rs"));
    draft.recheck(path("src/typechanged.rs"));

    let plan = built(draft);

    assert_eq!(
        paths(plan.recheck()),
        ["src/added.rs", "src/modified.rs", "src/typechanged.rs"]
    );
    assert!(plan.remove().is_empty());
}

#[test]
fn a_deletion_removes_and_a_rename_decomposes_into_both() {
    let mut draft = PlanDraft::new();
    draft.remove(path("src/gone.rs"));
    draft.rename(path("src/old.rs"), path("src/new.rs"));

    let plan = built(draft);

    assert_eq!(paths(plan.recheck()), ["src/new.rs"]);
    assert_eq!(paths(plan.remove()), ["src/gone.rs", "src/old.rs"]);
    assert_eq!(plan.renames().len(), 1);
    assert_eq!(plan.renames()[0].0.as_str(), "src/old.rs");
    assert_eq!(plan.renames()[0].1.as_str(), "src/new.rs");
}

#[test]
fn a_copy_leaves_its_source_alone() {
    let mut draft = PlanDraft::new();
    draft.copy(path("src/copy.rs"));

    let plan = built(draft);

    assert_eq!(paths(plan.recheck()), ["src/copy.rs"]);
    assert!(
        plan.remove().is_empty(),
        "a copy must never be read as a deletion"
    );
    assert!(plan.may_retain(&path("src/original.rs")));
}

#[test]
fn a_conflict_is_rechecked_and_reported() {
    let mut draft = PlanDraft::new();
    draft.conflict(path("src/merge.rs"));

    let plan = built(draft);

    assert_eq!(paths(plan.conflicted()), ["src/merge.rs"]);
    assert_eq!(paths(plan.recheck()), ["src/merge.rs"]);
    assert!(
        !plan.is_empty_delta(),
        "conflict stages cannot reach the no-op path"
    );
}

#[test]
fn a_staged_deletion_recreated_as_an_untracked_file_keeps_the_worktree_entry() {
    let mut draft = PlanDraft::new();
    draft.remove(path("src/token.rs"));
    draft.recheck(path("src/token.rs"));

    let plan = built(draft);

    assert_eq!(paths(plan.recheck()), ["src/token.rs"]);
    assert!(
        plan.remove().is_empty(),
        "the file is present, so it must be evaluated rather than dropped"
    );
}

#[test]
fn recheck_outranks_remove_whichever_order_status_reported_them_in() {
    let mut forwards = PlanDraft::new();
    forwards.recheck(path("src/token.rs"));
    forwards.remove(path("src/token.rs"));

    let mut backwards = PlanDraft::new();
    backwards.remove(path("src/token.rs"));
    backwards.recheck(path("src/token.rs"));

    assert_eq!(built(forwards), built(backwards));
}

#[test]
fn a_rename_source_that_is_immediately_recreated_is_not_a_contradiction() {
    // `git mv a b` and then a fresh untracked `a`: the move away from `a` and a
    // worktree entry at `a` are both true.
    let mut draft = PlanDraft::new();
    draft.rename(path("src/a.rs"), path("src/b.rs"));
    draft.recheck(path("src/a.rs"));

    let plan = built(draft);

    assert_eq!(paths(plan.recheck()), ["src/a.rs", "src/b.rs"]);
    assert!(plan.remove().is_empty());
    assert_eq!(plan.renames().len(), 1);
}

// --- normalization ----------------------------------------------------------

#[test]
fn status_order_and_repetition_cannot_reach_the_plan() {
    let mut shuffled = PlanDraft::new();
    for entry in ["src/z.rs", "src/a.rs", "src/m.rs", "src/a.rs"] {
        shuffled.recheck(path(entry));
    }
    shuffled.remove(path("src/gone.rs"));
    shuffled.remove(path("src/gone.rs"));

    let mut ordered = PlanDraft::new();
    for entry in ["src/a.rs", "src/m.rs", "src/z.rs"] {
        ordered.recheck(path(entry));
    }
    ordered.remove(path("src/gone.rs"));

    let plan = built(shuffled);
    assert_eq!(plan, built(ordered));
    assert_eq!(paths(plan.recheck()), ["src/a.rs", "src/m.rs", "src/z.rs"]);
    assert_eq!(paths(plan.remove()), ["src/gone.rs"]);
}

#[test]
fn only_untouched_paths_may_keep_their_recorded_facts() {
    let mut draft = PlanDraft::new();
    draft.recheck(path("src/edited.rs"));
    draft.remove(path("src/gone.rs"));

    let plan = built(draft);

    assert!(plan.may_retain(&path("src/untouched.rs")));
    assert!(!plan.may_retain(&path("src/edited.rs")));
    assert!(!plan.may_retain(&path("src/gone.rs")));
}

// --- contradictions ---------------------------------------------------------

#[test]
fn a_self_rename_is_rejected() {
    let mut draft = PlanDraft::new();
    draft.rename(path("src/same.rs"), path("src/same.rs"));

    let error = draft.build().expect_err("a self-rename must not build");
    assert!(
        matches!(error, RefreshError::InvalidRefreshPlan { ref path, .. } if path.as_str() == "src/same.rs"),
        "unexpected error: {error}"
    );
}

#[test]
fn two_sources_claiming_one_target_are_rejected() {
    let mut draft = PlanDraft::new();
    draft.rename(path("src/a.rs"), path("src/target.rs"));
    draft.rename(path("src/b.rs"), path("src/target.rs"));

    let error = draft.build().expect_err("two sources must not build");
    assert!(matches!(error, RefreshError::InvalidRefreshPlan { .. }));
}

#[test]
fn one_source_moving_to_two_targets_is_rejected() {
    let mut draft = PlanDraft::new();
    draft.rename(path("src/a.rs"), path("src/one.rs"));
    draft.rename(path("src/a.rs"), path("src/two.rs"));

    let error = draft.build().expect_err("a forked rename must not build");
    assert!(
        matches!(error, RefreshError::InvalidRefreshPlan { ref path, .. } if path.as_str() == "src/a.rs"),
        "unexpected error: {error}"
    );
}

#[test]
fn the_first_contradiction_is_the_one_reported() {
    let mut draft = PlanDraft::new();
    draft.rename(path("src/first.rs"), path("src/first.rs"));
    draft.rename(path("src/second.rs"), path("src/second.rs"));

    let error = draft.build().expect_err("a self-rename must not build");
    assert!(
        matches!(error, RefreshError::InvalidRefreshPlan { ref path, .. } if path.as_str() == "src/first.rs"),
        "unexpected error: {error}"
    );
}

// --- discovery identity -----------------------------------------------------

fn digest(walk: &WalkCfg) -> [u8; 32] {
    DiscoveryIdentity::new(walk).finish()
}

#[test]
fn the_identity_is_stable_for_the_same_rules() {
    let walk = WalkCfg::default();
    assert_eq!(digest(&walk), digest(&WalkCfg::default()));
}

#[test]
fn every_membership_rule_moves_the_identity() {
    let base = digest(&WalkCfg::default());

    let variants = [
        WalkCfg {
            use_default_excludes: false,
            ..WalkCfg::default()
        },
        WalkCfg {
            standard_filters: false,
            ..WalkCfg::default()
        },
        WalkCfg {
            follow_symlinks: true,
            ..WalkCfg::default()
        },
        WalkCfg {
            detect_generated: false,
            ..WalkCfg::default()
        },
        WalkCfg {
            custom_excludes: vec!["docs".to_owned()],
            ..WalkCfg::default()
        },
        WalkCfg {
            languages: Some(vec![Lang::Rust]),
            ..WalkCfg::default()
        },
        WalkCfg {
            max_files: Some(10),
            ..WalkCfg::default()
        },
    ];

    for variant in variants {
        assert_ne!(
            digest(&variant),
            base,
            "rule change did not move: {variant:?}"
        );
    }
}

#[test]
fn thread_count_is_not_a_membership_rule() {
    let single = WalkCfg {
        threads: Some(1),
        ..WalkCfg::default()
    };
    let many = WalkCfg {
        threads: Some(16),
        ..WalkCfg::default()
    };

    assert_eq!(digest(&single), digest(&many));
}

#[test]
fn an_allowlist_is_a_set_not_a_sequence() {
    let ordered = WalkCfg {
        languages: Some(vec![Lang::Rust, Lang::Rust]),
        ..WalkCfg::default()
    };
    let single = WalkCfg {
        languages: Some(vec![Lang::Rust]),
        ..WalkCfg::default()
    };

    assert_eq!(digest(&ordered), digest(&single));
}

#[test]
fn an_out_of_tree_rule_file_moves_the_identity_when_it_appears_or_changes() {
    let walk = WalkCfg::default();
    let mixed = |contents: Option<&[u8]>| {
        let mut identity = DiscoveryIdentity::new(&walk);
        identity.mix_rule_file("info-exclude", contents);
        identity.finish()
    };

    let absent = mixed(None);
    let empty = mixed(Some(b""));
    let rule = mixed(Some(b"*.rs\n"));
    let other = mixed(Some(b"*.md\n"));

    assert_ne!(absent, empty, "an empty file is not the same as no file");
    assert_ne!(empty, rule);
    assert_ne!(rule, other);
}

#[test]
fn labelled_fields_cannot_be_confused_with_each_other() {
    let walk = WalkCfg::default();
    let split = |first: &[u8], second: &[u8]| {
        let mut identity = DiscoveryIdentity::new(&walk);
        identity.mix_rule_file("a", Some(first));
        identity.mix_rule_file("b", Some(second));
        identity.finish()
    };

    assert_ne!(split(b"xy", b"z"), split(b"x", b"yz"));
}

// --- published spellings ----------------------------------------------------

#[test]
fn text_and_json_spell_every_enum_the_same_way() {
    #[track_caller]
    fn same<T: serde::Serialize + std::fmt::Debug>(value: T, as_str: &str) {
        let json = serde_json::to_string(&value).unwrap_or_default();
        assert_eq!(json, format!("\"{as_str}\""), "mismatch for {value:?}");
    }

    for reason in [
        FullReason::MissingSnapshot,
        FullReason::IncompatibleSnapshot,
        FullReason::CorruptSnapshot,
        FullReason::HeadChanged,
        FullReason::DiscoveryRulesChanged,
        FullReason::GitStatusUnavailable,
    ] {
        same(reason, reason.as_str());
    }
    for mode in [
        ReportedMode::Incremental,
        ReportedMode::Full,
        ReportedMode::FallbackFull,
    ] {
        same(mode, mode.as_str());
    }
    for outcome in [RefreshOutcome::Unchanged, RefreshOutcome::Updated] {
        same(outcome, outcome.as_str());
    }
    for label in [
        GitLabel::Clean,
        GitLabel::Dirty,
        GitLabel::Conflicted,
        GitLabel::Unavailable,
        GitLabel::NoGit,
    ] {
        same(label, label.as_str());
    }
    for label in [
        SnapshotLabel::Fresh,
        SnapshotLabel::Stale,
        SnapshotLabel::Missing,
        SnapshotLabel::Corrupt,
        SnapshotLabel::Incompatible,
        SnapshotLabel::Unknown,
    ] {
        same(label, label.as_str());
    }
}

// --- rendering --------------------------------------------------------------

#[test]
fn an_unchanged_refresh_reports_the_work_it_did_not_do() {
    let report = RefreshReport {
        elapsed_ms: 7,
        ..RefreshReport::default()
    };

    assert_eq!(
        render_refresh_text(&report, RefreshCommand::Refresh),
        "rr refresh — unchanged, 0 reparsed, 0 content reads (7 ms)"
    );
}

#[test]
fn an_updated_refresh_names_only_the_counters_with_something_to_say() {
    let report = RefreshReport {
        outcome: RefreshOutcome::Updated,
        changed: 1,
        reparsed: 1,
        cached: 41,
        content_reads: 2,
        removed: 1,
        snapshot_updated: true,
        elapsed_ms: 24,
        ..RefreshReport::default()
    };

    assert_eq!(
        render_refresh_text(&report, RefreshCommand::Refresh),
        "rr refresh — updated, 1 reparsed, 41 cached, 1 removed (24 ms)"
    );
}

#[test]
fn a_fallback_names_its_reason_in_both_renderings() {
    let report = RefreshReport {
        outcome: RefreshOutcome::Updated,
        mode: ReportedMode::FallbackFull,
        fallback_reason: Some(FullReason::HeadChanged),
        cached: 42,
        snapshot_updated: true,
        elapsed_ms: 81,
        ..RefreshReport::default()
    };

    assert_eq!(
        render_refresh_text(&report, RefreshCommand::Refresh),
        "rr refresh — updated (full fallback: HEAD changed), 0 reparsed, 42 cached (81 ms)"
    );

    let json = render_refresh_json(&report, RefreshCommand::Refresh).unwrap_or_default();
    assert!(json.contains(r#""mode":"fallback-full""#), "{json}");
    assert!(
        json.contains(r#""fallback_reason":"head-changed""#),
        "{json}"
    );
    assert!(json.contains(r#""schema_version":1"#), "{json}");
    assert!(json.contains(r#""command":"refresh""#), "{json}");
    assert!(!json.contains('\n'), "JSON must be one compact object");
}

#[test]
fn the_json_object_carries_every_documented_field() {
    let json =
        render_refresh_json(&RefreshReport::default(), RefreshCommand::Map).unwrap_or_default();

    for field in [
        "schema_version",
        "command",
        "outcome",
        "mode",
        "fallback_reason",
        "changed",
        "reparsed",
        "cached",
        "cache_corrupt",
        "content_reads",
        "removed",
        "renamed",
        "degraded",
        "conflicted",
        "snapshot_updated",
        "elapsed_ms",
    ] {
        assert!(
            json.contains(&format!("\"{field}\"")),
            "{field} missing: {json}"
        );
    }
    assert!(json.contains(r#""command":"map""#), "{json}");
    assert!(json.contains(r#""fallback_reason":null"#), "{json}");
}

#[test]
fn status_text_pluralizes_and_abbreviates_like_git_does() {
    let head = crate::oid::Oid::from_hex("8f3a2c1000000000000000000000000000000000").ok();
    let report = StatusReport {
        git: GitLabel::Dirty,
        head,
        snapshot: SnapshotLabel::Stale,
        stale_paths: Some(1),
        unresolved: 12,
    };

    assert_eq!(
        render_status_text(&report),
        "git: dirty @ 8f3a2c1 · snapshot: stale (1 path) · unresolved: 12"
    );

    let many = StatusReport {
        stale_paths: Some(4),
        ..report
    };
    assert!(render_status_text(&many).contains("stale (4 paths)"));
}

#[test]
fn unknowable_staleness_is_never_rendered_as_fresh() {
    let report = StatusReport {
        git: GitLabel::Unavailable,
        head: None,
        snapshot: SnapshotLabel::Unknown,
        stale_paths: None,
        unresolved: 0,
    };

    let text = render_status_text(&report);
    assert_eq!(text, "git: unavailable · snapshot: unknown · unresolved: 0");
    assert!(!text.contains("fresh"));

    let json = render_status_json(&report).unwrap_or_default();
    assert!(json.contains(r#""stale_paths":null"#), "{json}");
    assert!(json.contains(r#""head":null"#), "{json}");
}
