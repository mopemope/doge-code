//! Directive-to-Evidence traceability integration tests (v1).
//!
//! Covers spec §116-136: schema compatibility, directive semantics,
//! requirement reducer, plan links, frozen change attribution, verification
//! snapshot semantics, requirement coverage, and budgeted reads.

#[cfg(test)]
#[allow(clippy::module_inception)]
mod traceability_tests {
    use crate::provenance::requirements::{
        compute_requirement_coverage, current_requirements, plan_requirement_links,
    };
    use crate::provenance::types::{
        PlanChangedEvent, PlanItemTransition, RequirementChangedEvent, RequirementSnapshot,
        RequirementStatus, RequirementTransition,
    };
    use crate::provenance::{
        ChangeCommittedEvent, ChangeKind, ChangeTarget, DirectiveOrigin, FileStateEvidence,
        ProvenanceAttribution, ProvenanceEvent, ProvenanceStore, VerificationContext,
        VerificationKind, VerificationObservedEvent, VerificationSource, file_content_hash,
    };
    use crate::tools::FsTools;

    fn fs_with_session(project_root: &std::path::Path) -> FsTools {
        let sessions_root = project_root.join(".doge/sessions");
        let store = crate::session::SessionStore::new(sessions_root).unwrap();
        let manager = std::sync::Arc::new(std::sync::Mutex::new(crate::session::SessionManager {
            save_state: Default::default(),
            current_lease: None,
            store,
            current_session: None,
        }));
        {
            let mut mgr = manager.lock().unwrap();
            mgr.create_session(None).unwrap();
        }
        let config = std::sync::Arc::new(crate::config::AppConfig {
            project_root: project_root.to_path_buf(),
            ..crate::config::AppConfig::default()
        });
        FsTools::new(std::sync::Arc::new(tokio::sync::RwLock::new(None)), config)
            .with_session_manager(manager)
    }

    fn plan_item(id: &str, status: &str, reqs: Vec<String>) -> crate::tools::plan::PlanItem {
        crate::tools::plan::PlanItem {
            id: id.to_string(),
            parent_id: None,
            content: format!("work {id}"),
            status: status.to_string(),
            requirement_ids: reqs,
            verification_obligations: Vec::new(),
        }
    }

    // --- Schema compatibility (§116) ---

    #[test]
    fn test_v3_roundtrip_directive_and_requirement() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(dir.path().join("s"));
        let d = store
            .append(
                "s",
                ProvenanceEvent::DirectiveObserved(crate::provenance::DirectiveObservedEvent {
                    origin: DirectiveOrigin::TuiPrompt,
                    raw_input: "hello".to_string(),
                    raw_input_hash: crate::provenance::directive_content_hash("hello"),
                    effective_instruction: "hello".to_string(),
                    effective_instruction_hash: crate::provenance::directive_content_hash("hello"),
                }),
            )
            .unwrap();
        assert_eq!(
            d.event_type(),
            crate::provenance::ProvenanceEventType::DirectiveObserved
        );
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 1);
        assert_eq!(loaded.events[0].directive_id(), Some(d.event_id.as_str()));
    }

    #[test]
    fn test_mixed_v1_v2_v3_queryable() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join("s");
        // v1
        let v1_dir = session_dir.join("provenance/v1/events");
        std::fs::create_dir_all(&v1_dir).unwrap();
        std::fs::write(
            v1_dir.join("v1-1.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": 1,
                "event_id": "v1-1",
                "session_id": "s",
                "timestamp": "2026-01-01T00:00:00+00:00",
                "event": {
                    "type": "change_committed",
                    "transaction_id": "",
                    "change_kind": "semantic_edit",
                    "file": "src/lib.rs",
                    "symbol_id": "sym",
                    "before_fingerprint": "a",
                    "after_fingerprint": "b",
                    "diff": "d",
                    "diff_hash": "blake3:x",
                    "lines_added": 1,
                    "lines_removed": 0
                }
            }))
            .unwrap(),
        )
        .unwrap();
        // v2
        let v2_dir = session_dir.join("provenance/v2/events");
        std::fs::create_dir_all(&v2_dir).unwrap();
        std::fs::write(
            v2_dir.join("v2-1.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": 2,
                "event_id": "v2-1",
                "session_id": "s",
                "timestamp": "2026-01-02T00:00:00+00:00",
                "event": {"type": "plan_changed", "changes": []}
            }))
            .unwrap(),
        )
        .unwrap();
        // v3
        let store = ProvenanceStore::new(session_dir);
        store
            .append(
                "s",
                ProvenanceEvent::DirectiveObserved(crate::provenance::DirectiveObservedEvent {
                    origin: DirectiveOrigin::ExecRun,
                    raw_input: "run".to_string(),
                    raw_input_hash: crate::provenance::directive_content_hash("run"),
                    effective_instruction: "run".to_string(),
                    effective_instruction_hash: crate::provenance::directive_content_hash("run"),
                }),
            )
            .unwrap();
        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.events.len(), 3);
        assert_eq!(loaded.events[0].event_id, "v1-1");
        assert_eq!(loaded.events[1].event_id, "v2-1");
    }

    // --- Directive semantics (§117) ---

    #[test]
    fn test_directive_plain_raw_equals_effective() {
        let proj = tempfile::tempdir().unwrap();
        let fs = fs_with_session(proj.path());
        let env = crate::tools::provenance::record_directive_observed(
            &fs,
            DirectiveOrigin::TuiPrompt,
            "Fix authentication cache",
            "Fix authentication cache",
        )
        .unwrap();
        match &env.event {
            ProvenanceEvent::DirectiveObserved(d) => {
                assert_eq!(d.raw_input, d.effective_instruction);
                assert_eq!(d.raw_input_hash, d.effective_instruction_hash);
                assert!(d.raw_input_hash.starts_with("blake3:"));
            }
            _ => panic!("expected directive"),
        }
        // Canonical id is the envelope event_id.
        assert_eq!(env.directive_id(), Some(env.event_id.as_str()));
    }

    #[test]
    fn test_directive_custom_raw_differs_effective() {
        let proj = tempfile::tempdir().unwrap();
        let fs = fs_with_session(proj.path());
        let env = crate::tools::provenance::record_directive_observed(
            &fs,
            DirectiveOrigin::TuiCustomCommand,
            "/fix-cache",
            "Expanded custom command: fix the cache with ...",
        )
        .unwrap();
        match &env.event {
            ProvenanceEvent::DirectiveObserved(d) => {
                assert_ne!(d.raw_input, d.effective_instruction);
                assert_ne!(d.raw_input_hash, d.effective_instruction_hash);
            }
            _ => panic!("expected directive"),
        }
    }

    #[test]
    fn test_directive_hashes_are_exact_bytes() {
        let h1 = crate::provenance::directive_content_hash("a\n");
        let h2 = crate::provenance::directive_content_hash("a\r\n");
        assert_ne!(h1, h2);
        assert!(h1.starts_with("blake3:"));
    }

    // --- Plan backward compat (§121) ---

    #[test]
    fn test_old_plan_json_deserializes_with_empty_requirements() {
        let json = serde_json::json!({
            "session_id": "s",
            "items": [{"id": "step-1", "content": "do", "status": "pending"}]
        });
        let list: crate::tools::plan::PlanList = serde_json::from_value(json).unwrap();
        assert_eq!(list.items[0].requirement_ids, Vec::<String>::new());
    }

    // --- Plan validation (§122) ---

    #[test]
    fn test_plan_write_rejects_unknown_requirement() {
        let proj = tempfile::tempdir().unwrap();
        let fs = fs_with_session(proj.path());
        // No requirements exist; any link must fail.
        let err = fs
            .plan_write(
                vec![plan_item("step-1", "pending", vec!["req-nope".to_string()])],
                crate::tools::plan::PlanWriteMode::Replace,
            )
            .unwrap_err();
        assert!(err.to_string().contains("unknown requirement"));
    }

    #[test]
    fn test_plan_write_accepts_known_requirement() {
        let proj = tempfile::tempdir().unwrap();
        let fs = fs_with_session(proj.path());
        crate::tools::requirements::requirements_write(
            &fs,
            crate::tools::requirements::RequirementsWriteArgs {
                upserts: vec![crate::tools::requirements::RequirementInput {
                    id: "req-a".to_string(),
                    statement: "do a".to_string(),
                }],
                withdraw_ids: vec![],
            },
            &ProvenanceAttribution::with_directive("d1"),
        )
        .unwrap();
        let res = fs
            .plan_write(
                vec![plan_item("step-1", "pending", vec!["req-a".to_string()])],
                crate::tools::plan::PlanWriteMode::Replace,
            )
            .unwrap();
        assert_eq!(res.plan.items[0].requirement_ids, vec!["req-a".to_string()]);
    }

    #[test]
    fn test_plan_write_warns_withdrawn_link() {
        let proj = tempfile::tempdir().unwrap();
        let fs = fs_with_session(proj.path());
        crate::tools::requirements::requirements_write(
            &fs,
            crate::tools::requirements::RequirementsWriteArgs {
                upserts: vec![crate::tools::requirements::RequirementInput {
                    id: "req-w".to_string(),
                    statement: "temp".to_string(),
                }],
                withdraw_ids: vec![],
            },
            &ProvenanceAttribution::with_directive("d1"),
        )
        .unwrap();
        fs.plan_write(
            vec![plan_item("step-1", "pending", vec!["req-w".to_string()])],
            crate::tools::plan::PlanWriteMode::Replace,
        )
        .unwrap();
        crate::tools::requirements::requirements_write(
            &fs,
            crate::tools::requirements::RequirementsWriteArgs {
                upserts: vec![],
                withdraw_ids: vec!["req-w".to_string()],
            },
            &ProvenanceAttribution::with_directive("d2"),
        )
        .unwrap();
        // Rewriting the same plan after withdrawal keeps the link but warns.
        let res = fs
            .plan_write(
                vec![plan_item("step-1", "pending", vec!["req-w".to_string()])],
                crate::tools::plan::PlanWriteMode::Replace,
            )
            .unwrap();
        // No-op (same content) means unchanged; force a status change to trigger warnings.
        let _ = res;
        let res2 = fs
            .plan_write(
                vec![plan_item(
                    "step-1",
                    "in_progress",
                    vec!["req-w".to_string()],
                )],
                crate::tools::plan::PlanWriteMode::Replace,
            )
            .unwrap();
        assert!(
            res2.warnings.iter().any(|w| w.contains("withdrawn")),
            "warnings: {:?}",
            res2.warnings
        );
    }

    // --- Plan transition (§123) ---

    #[test]
    fn test_requirement_link_only_change_generates_plan_changed() {
        let before = vec![plan_item("step-1", "pending", vec![])];
        let after = vec![plan_item("step-1", "pending", vec!["r1".to_string()])];
        let t = crate::tools::provenance::diff_plan_transitions(&before, &after);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].before_requirement_ids, Vec::<String>::new());
        assert_eq!(t[0].after_requirement_ids, vec!["r1".to_string()]);
    }

    // --- Change attribution (§124-126) ---

    fn commit_file(
        fs: &FsTools,
        path: &std::path::Path,
        content: &str,
        attribution: &ProvenanceAttribution,
    ) -> String {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let rel = path
                .strip_prefix(&fs.config.project_root)
                .unwrap()
                .to_string_lossy()
                .to_string();
            let _ = rel;
            fs.fs_write_with_attribution(path.to_str().unwrap(), content, attribution)
                .await
                .unwrap();
        });
        let loaded = crate::tools::provenance::load_current_events(fs)
            .unwrap()
            .unwrap();
        loaded.events.last().unwrap().event_id.clone()
    }

    #[test]
    fn test_planned_mutation_freezes_directive_and_requirements() {
        let proj = tempfile::tempdir().unwrap();
        let fs = fs_with_session(proj.path());
        let d = crate::tools::provenance::record_directive_observed(
            &fs,
            DirectiveOrigin::TuiPrompt,
            "Add cache",
            "Add cache",
        )
        .unwrap();
        let attr = ProvenanceAttribution::with_directive(d.event_id.clone());
        crate::tools::requirements::requirements_write(
            &fs,
            crate::tools::requirements::RequirementsWriteArgs {
                upserts: vec![crate::tools::requirements::RequirementInput {
                    id: "r1".to_string(),
                    statement: "cache auth".to_string(),
                }],
                withdraw_ids: vec![],
            },
            &attr,
        )
        .unwrap();
        fs.plan_write_with_attribution(
            vec![plan_item("p1", "in_progress", vec!["r1".to_string()])],
            crate::tools::plan::PlanWriteMode::Replace,
            &attr,
        )
        .unwrap();
        let file = proj.path().join("a.txt");
        std::fs::write(&file, "v0\n").unwrap();
        commit_file(&fs, &file, "v1\n", &attr);
        let loaded = crate::tools::provenance::load_current_events(&fs)
            .unwrap()
            .unwrap();
        let change = loaded
            .events
            .iter()
            .find_map(|e| match &e.event {
                ProvenanceEvent::ChangeCommitted(c) => Some((e, c)),
                _ => None,
            })
            .unwrap();
        assert_eq!(change.1.directive_id.as_deref(), Some(d.event_id.as_str()));
        assert_eq!(change.1.plan_item_id.as_deref(), Some("p1"));
        assert_eq!(change.1.requirement_ids, vec!["r1".to_string()]);
    }

    #[test]
    fn test_unplanned_mutation_carries_directive_only() {
        let proj = tempfile::tempdir().unwrap();
        let fs = fs_with_session(proj.path());
        let d = crate::tools::provenance::record_directive_observed(
            &fs,
            DirectiveOrigin::TuiPrompt,
            "quick fix",
            "quick fix",
        )
        .unwrap();
        let attr = ProvenanceAttribution::with_directive(d.event_id.clone());
        // No plan at all.
        let file = proj.path().join("a.txt");
        std::fs::write(&file, "v0\n").unwrap();
        commit_file(&fs, &file, "v1\n", &attr);
        let loaded = crate::tools::provenance::load_current_events(&fs)
            .unwrap()
            .unwrap();
        let c = loaded
            .events
            .iter()
            .find_map(|e| match &e.event {
                ProvenanceEvent::ChangeCommitted(c) => Some(c),
                _ => None,
            })
            .unwrap();
        assert_eq!(c.directive_id.as_deref(), Some(d.event_id.as_str()));
        assert_eq!(c.plan_item_id, None);
        assert!(c.requirement_ids.is_empty());
    }

    #[test]
    fn test_frozen_attribution_survives_plan_remap() {
        let proj = tempfile::tempdir().unwrap();
        let fs = fs_with_session(proj.path());
        let d = crate::tools::provenance::record_directive_observed(
            &fs,
            DirectiveOrigin::TuiPrompt,
            "work",
            "work",
        )
        .unwrap();
        let attr = ProvenanceAttribution::with_directive(d.event_id.clone());
        for (id, stmt) in [("r1", "first"), ("r2", "second")] {
            crate::tools::requirements::requirements_write(
                &fs,
                crate::tools::requirements::RequirementsWriteArgs {
                    upserts: vec![crate::tools::requirements::RequirementInput {
                        id: id.to_string(),
                        statement: stmt.to_string(),
                    }],
                    withdraw_ids: vec![],
                },
                &attr,
            )
            .unwrap();
        }
        fs.plan_write_with_attribution(
            vec![plan_item("p1", "in_progress", vec!["r1".to_string()])],
            crate::tools::plan::PlanWriteMode::Replace,
            &attr,
        )
        .unwrap();
        let file = proj.path().join("a.txt");
        std::fs::write(&file, "v0\n").unwrap();
        commit_file(&fs, &file, "v1\n", &attr);
        // Remap the plan item to R2.
        fs.plan_write_with_attribution(
            vec![plan_item("p1", "in_progress", vec!["r2".to_string()])],
            crate::tools::plan::PlanWriteMode::Replace,
            &attr,
        )
        .unwrap();
        let loaded = crate::tools::provenance::load_current_events(&fs)
            .unwrap()
            .unwrap();
        let c = loaded
            .events
            .iter()
            .find_map(|e| match &e.event {
                ProvenanceEvent::ChangeCommitted(c) => Some(c),
                _ => None,
            })
            .unwrap();
        assert_eq!(c.requirement_ids, vec!["r1".to_string()]);
    }

    // --- Verification attribution (§127-129) ---

    #[test]
    fn test_verification_freezes_requirement_union() {
        // C1 -> R1, C2 -> R2, then verification observes both.
        let proj = tempfile::tempdir().unwrap();
        let fs = fs_with_session(proj.path());
        let d = crate::tools::provenance::record_directive_observed(
            &fs,
            DirectiveOrigin::TuiPrompt,
            "work",
            "work",
        )
        .unwrap();
        let attr = ProvenanceAttribution::with_directive(d.event_id.clone());
        for (id, stmt) in [("r1", "one"), ("r2", "two")] {
            crate::tools::requirements::requirements_write(
                &fs,
                crate::tools::requirements::RequirementsWriteArgs {
                    upserts: vec![crate::tools::requirements::RequirementInput {
                        id: id.to_string(),
                        statement: stmt.to_string(),
                    }],
                    withdraw_ids: vec![],
                },
                &attr,
            )
            .unwrap();
        }
        fs.plan_write_with_attribution(
            vec![plan_item("p1", "in_progress", vec!["r1".to_string()])],
            crate::tools::plan::PlanWriteMode::Replace,
            &attr,
        )
        .unwrap();
        let f1 = proj.path().join("a.txt");
        let f2 = proj.path().join("b.txt");
        std::fs::write(&f1, "v0\n").unwrap();
        std::fs::write(&f2, "v0\n").unwrap();
        commit_file(&fs, &f1, "v1\n", &attr);
        fs.plan_write_with_attribution(
            vec![plan_item("p1", "in_progress", vec!["r2".to_string()])],
            crate::tools::plan::PlanWriteMode::Replace,
            &attr,
        )
        .unwrap();
        commit_file(&fs, &f2, "v1\n", &attr);
        let loaded = crate::tools::provenance::load_current_events(&fs)
            .unwrap()
            .unwrap();
        let active = crate::provenance::active_change_ids(&fs.config.project_root, &loaded.events);
        assert_eq!(active.len(), 2);
        // Build context with frozen requirement union.
        let change_reqs =
            crate::tools::requirements::requirement_ids_for_changes(&loaded.events, &active);
        let ctx = crate::provenance::verification::capture_verification_context_full(
            &fs.plan_read().unwrap().items,
            &active,
            &change_reqs,
            attr.directive_id.clone(),
            &std::collections::HashMap::new(),
        );
        assert_eq!(ctx.directive_id, attr.directive_id);
        assert!(ctx.requirement_ids.contains(&"r1".to_string()));
        assert!(ctx.requirement_ids.contains(&"r2".to_string()));
        assert_eq!(ctx.observed_change_ids, active);
    }

    #[test]
    fn test_verification_race_keeps_capture_only() {
        let ctx_a = VerificationContext {
            execution_context: None,
            execution_workspace: None,
            directive_id: Some("d1".to_string()),
            plan_item_id: Some("step-1".to_string()),
            requirement_ids: vec!["r1".to_string()],
            observed_change_ids: vec!["change-A".to_string()],
            matched_obligations: Vec::new(),
        };
        let event = crate::provenance::build_verification_event(
            crate::provenance::VerificationRecordInput {
                structured_test_result: None,
                kind: VerificationKind::Test,
                source: VerificationSource::ExecuteProcess,
                program: "cargo",
                args: &["test".to_string()],
                cwd_relative: None,
                success: true,
                status: "completed",
                exit_code: Some(0),
                timed_out: false,
                stdout: "ok",
                stderr: "",
                capture_truncated: false,
                context: ctx_a,
                extra_warnings: vec![],
            },
        );
        assert_eq!(event.observed_change_ids, vec!["change-A".to_string()]);
        assert_eq!(event.requirement_ids, vec!["r1".to_string()]);
        assert_eq!(event.directive_id.as_deref(), Some("d1"));
    }

    #[test]
    fn test_failed_verification_keeps_link_but_not_passing() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("a.txt"), "h1\n").unwrap();
        let sess = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(sess.path().join("s"));
        let before = FileStateEvidence {
            exists: true,
            content_hash: Some(file_content_hash("h0\n")),
            byte_len: Some(3),
        };
        let after = FileStateEvidence {
            exists: true,
            content_hash: Some(file_content_hash("h1\n")),
            byte_len: Some(3),
        };
        let change = store
            .append(
                "s",
                ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                    transaction_id: String::new(),
                    directive_id: Some("d1".to_string()),
                    plan_item_id: None,
                    requirement_ids: vec!["r1".to_string()],
                    change_kind: ChangeKind::TextEdit,
                    file: "a.txt".to_string(),
                    target: ChangeTarget::File,
                    before,
                    after,
                    predecessor_change_id: None,
                    reverts_change_id: None,
                    diff: "d".to_string(),
                    diff_hash: "blake3:x".to_string(),
                    lines_added: 1,
                    lines_removed: 0,
                }),
            )
            .unwrap();
        let failed = VerificationObservedEvent {
            structured_test_result: None,
            execution_context: None,
            execution_workspace: None,
            directive_id: Some("d1".to_string()),
            plan_item_id: None,
            requirement_ids: vec!["r1".to_string()],
            verification_kind: VerificationKind::Test,
            source: VerificationSource::ExecuteProcess,
            command: crate::provenance::CommandEvidence {
                program: "cargo".to_string(),
                args: vec!["test".to_string()],
                cwd: None,
            },
            outcome: crate::provenance::VerificationOutcome {
                success: false,
                status: "completed".to_string(),
                exit_code: Some(1),
                timed_out: false,
            },
            observed_change_ids: vec![change.event_id.clone()],
            matched_obligations: Vec::new(),
            stdout_excerpt: String::new(),
            stderr_excerpt: String::new(),
            output_digest: "blake3:x".to_string(),
            output_truncated: false,
            warnings: vec![],
        };
        store
            .append("s", ProvenanceEvent::VerificationObserved(failed))
            .unwrap();
        let loaded = store.load_all().unwrap();
        // Failed verification still carries the requirement link.
        let v = loaded
            .events
            .iter()
            .find_map(|e| match &e.event {
                ProvenanceEvent::VerificationObserved(v) => Some(v),
                _ => None,
            })
            .unwrap();
        assert_eq!(v.requirement_ids, vec!["r1".to_string()]);
        // But coverage must not be ObservedPassing.
        let reqs = current_requirements(&loaded.events);
        assert!(reqs.items.is_empty()); // no RequirementChanged yet
        let coverages = compute_requirement_coverage(
            &loaded.events,
            &[crate::provenance::requirements::CurrentRequirement {
                id: "r1".to_string(),
                statement: "s".to_string(),
                status: RequirementStatus::Active,
                source_directive_ids: vec!["d1".to_string()],
            }],
            &std::collections::HashMap::new(),
            proj.path(),
        );
        assert_ne!(
            coverages[0].evidence_state,
            crate::provenance::requirements::RequirementEvidenceState::ObservedPassing
        );
    }

    // --- Requirement coverage (§130-134) ---

    #[test]
    fn test_coverage_observed_passing() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("a.txt"), "h1\n").unwrap();
        let sess = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(sess.path().join("s"));
        let mk_change = |reqs: Vec<String>| {
            store
                .append(
                    "s",
                    ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                        transaction_id: String::new(),
                        directive_id: Some("d1".to_string()),
                        plan_item_id: Some("p1".to_string()),
                        requirement_ids: reqs,
                        change_kind: ChangeKind::TextEdit,
                        file: "a.txt".to_string(),
                        target: ChangeTarget::File,
                        before: FileStateEvidence {
                            exists: true,
                            content_hash: Some(file_content_hash("h0\n")),
                            byte_len: Some(3),
                        },
                        after: FileStateEvidence {
                            exists: true,
                            content_hash: Some(file_content_hash("h1\n")),
                            byte_len: Some(3),
                        },
                        predecessor_change_id: None,
                        reverts_change_id: None,
                        diff: "d".to_string(),
                        diff_hash: "blake3:x".to_string(),
                        lines_added: 1,
                        lines_removed: 0,
                    }),
                )
                .unwrap()
        };
        let c1 = mk_change(vec!["r1".to_string()]);
        // Plan link R1 -> P1.
        store
            .append(
                "s",
                ProvenanceEvent::PlanChanged(PlanChangedEvent {
                    directive_id: Some("d1".to_string()),
                    changes: vec![PlanItemTransition {
                        plan_item_id: "p1".to_string(),
                        parent_id: None,
                        content: "work".to_string(),
                        before_status: None,
                        after_status: Some("in_progress".to_string()),
                        before_requirement_ids: vec![],
                        before_verification_obligations: Vec::new(),
                        after_requirement_ids: vec!["r1".to_string()],
                        after_verification_obligations: Vec::new(),
                    }],
                }),
            )
            .unwrap();
        let ctx = VerificationContext {
            execution_context: None,
            execution_workspace: None,
            directive_id: Some("d1".to_string()),
            plan_item_id: Some("p1".to_string()),
            requirement_ids: vec!["r1".to_string()],
            observed_change_ids: vec![c1.event_id.clone()],
            matched_obligations: Vec::new(),
        };
        let v = crate::provenance::build_verification_event(
            crate::provenance::VerificationRecordInput {
                structured_test_result: None,
                kind: VerificationKind::Test,
                source: VerificationSource::ExecuteProcess,
                program: "cargo",
                args: &["test".to_string()],
                cwd_relative: None,
                success: true,
                status: "completed",
                exit_code: Some(0),
                timed_out: false,
                stdout: "ok",
                stderr: "",
                capture_truncated: false,
                context: ctx,
                extra_warnings: vec![],
            },
        );
        store
            .append("s", ProvenanceEvent::VerificationObserved(v))
            .unwrap();
        let loaded = store.load_all().unwrap();
        let mut links = std::collections::HashMap::new();
        links.insert("p1".to_string(), vec!["r1".to_string()]);
        let cov = compute_requirement_coverage(
            &loaded.events,
            &[crate::provenance::requirements::CurrentRequirement {
                id: "r1".to_string(),
                statement: "s".to_string(),
                status: RequirementStatus::Active,
                source_directive_ids: vec!["d1".to_string()],
            }],
            &links,
            proj.path(),
        );
        assert_eq!(cov[0].active_change_ids, vec![c1.event_id.clone()]);
        assert_eq!(cov[0].verified_active_change_ids, vec![c1.event_id.clone()]);
        assert_eq!(
            cov[0].evidence_state,
            crate::provenance::requirements::RequirementEvidenceState::ObservedPassing
        );
    }

    #[test]
    fn test_coverage_active_unverified() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("a.txt"), "h1\n").unwrap();
        let sess = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(sess.path().join("s"));
        let c1 = store
            .append(
                "s",
                ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                    transaction_id: String::new(),
                    directive_id: Some("d1".to_string()),
                    plan_item_id: Some("p1".to_string()),
                    requirement_ids: vec!["r1".to_string()],
                    change_kind: ChangeKind::TextEdit,
                    file: "a.txt".to_string(),
                    target: ChangeTarget::File,
                    before: FileStateEvidence {
                        exists: true,
                        content_hash: Some(file_content_hash("h0\n")),
                        byte_len: Some(3),
                    },
                    after: FileStateEvidence {
                        exists: true,
                        content_hash: Some(file_content_hash("h1\n")),
                        byte_len: Some(3),
                    },
                    predecessor_change_id: None,
                    reverts_change_id: None,
                    diff: "d".to_string(),
                    diff_hash: "blake3:x".to_string(),
                    lines_added: 1,
                    lines_removed: 0,
                }),
            )
            .unwrap();
        let loaded = store.load_all().unwrap();
        let mut links = std::collections::HashMap::new();
        links.insert("p1".to_string(), vec!["r1".to_string()]);
        let cov = compute_requirement_coverage(
            &loaded.events,
            &[crate::provenance::requirements::CurrentRequirement {
                id: "r1".to_string(),
                statement: "s".to_string(),
                status: RequirementStatus::Active,
                source_directive_ids: vec!["d1".to_string()],
            }],
            &links,
            proj.path(),
        );
        assert_eq!(
            cov[0].evidence_state,
            crate::provenance::requirements::RequirementEvidenceState::ActiveUnverified
        );
        assert!(cov[0].verified_active_change_ids.is_empty());
        assert_eq!(cov[0].active_change_ids, vec![c1.event_id]);
    }

    #[test]
    fn test_coverage_mixed_verified_and_unverified() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join("b.txt"), "h2\n").unwrap();
        std::fs::write(proj.path().join("a.txt"), "h1\n").unwrap();
        let sess = tempfile::tempdir().unwrap();
        let store = ProvenanceStore::new(sess.path().join("s"));
        // Two changes for R1 on different files (both stay active).
        let c1 = store
            .append(
                "s",
                ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                    transaction_id: String::new(),
                    directive_id: Some("d1".to_string()),
                    plan_item_id: Some("p1".to_string()),
                    requirement_ids: vec!["r1".to_string()],
                    change_kind: ChangeKind::TextEdit,
                    file: "a.txt".to_string(),
                    target: ChangeTarget::File,
                    before: FileStateEvidence {
                        exists: true,
                        content_hash: Some(file_content_hash("h0\n")),
                        byte_len: Some(3),
                    },
                    after: FileStateEvidence {
                        exists: true,
                        content_hash: Some(file_content_hash("h1\n")),
                        byte_len: Some(3),
                    },
                    predecessor_change_id: None,
                    reverts_change_id: None,
                    diff: "d".to_string(),
                    diff_hash: "blake3:x".to_string(),
                    lines_added: 1,
                    lines_removed: 0,
                }),
            )
            .unwrap();
        let c2 = store
            .append(
                "s",
                ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                    transaction_id: String::new(),
                    directive_id: Some("d1".to_string()),
                    plan_item_id: Some("p1".to_string()),
                    requirement_ids: vec!["r1".to_string()],
                    change_kind: ChangeKind::TextEdit,
                    file: "b.txt".to_string(),
                    target: ChangeTarget::File,
                    before: FileStateEvidence {
                        exists: true,
                        content_hash: Some(file_content_hash("g0\n")),
                        byte_len: Some(3),
                    },
                    after: FileStateEvidence {
                        exists: true,
                        content_hash: Some(file_content_hash("h2\n")),
                        byte_len: Some(3),
                    },
                    predecessor_change_id: None,
                    reverts_change_id: None,
                    diff: "d".to_string(),
                    diff_hash: "blake3:x".to_string(),
                    lines_added: 1,
                    lines_removed: 0,
                }),
            )
            .unwrap();
        // Verify only C1.
        let ctx = VerificationContext {
            execution_context: None,
            execution_workspace: None,
            directive_id: Some("d1".to_string()),
            plan_item_id: Some("p1".to_string()),
            requirement_ids: vec!["r1".to_string()],
            observed_change_ids: vec![c1.event_id.clone()],
            matched_obligations: Vec::new(),
        };
        let v = crate::provenance::build_verification_event(
            crate::provenance::VerificationRecordInput {
                structured_test_result: None,
                kind: VerificationKind::Test,
                source: VerificationSource::ExecuteProcess,
                program: "cargo",
                args: &["test".to_string()],
                cwd_relative: None,
                success: true,
                status: "completed",
                exit_code: Some(0),
                timed_out: false,
                stdout: "ok",
                stderr: "",
                capture_truncated: false,
                context: ctx,
                extra_warnings: vec![],
            },
        );
        store
            .append("s", ProvenanceEvent::VerificationObserved(v))
            .unwrap();
        let loaded = store.load_all().unwrap();
        let mut links = std::collections::HashMap::new();
        links.insert("p1".to_string(), vec!["r1".to_string()]);
        let cov = compute_requirement_coverage(
            &loaded.events,
            &[crate::provenance::requirements::CurrentRequirement {
                id: "r1".to_string(),
                statement: "s".to_string(),
                status: RequirementStatus::Active,
                source_directive_ids: vec!["d1".to_string()],
            }],
            &links,
            proj.path(),
        );
        assert_eq!(
            cov[0].evidence_state,
            crate::provenance::requirements::RequirementEvidenceState::Mixed
        );
        assert_eq!(cov[0].verified_active_change_ids, vec![c1.event_id]);
        assert_eq!(cov[0].unverified_active_change_ids, vec![c2.event_id]);
    }

    // --- provenance_read filters (§135) ---

    #[tokio::test]
    async fn test_provenance_read_directive_and_requirement_filters() {
        let proj = tempfile::tempdir().unwrap();
        let fs = fs_with_session(proj.path());
        let d1 = crate::tools::provenance::record_directive_observed(
            &fs,
            DirectiveOrigin::TuiPrompt,
            "first",
            "first",
        )
        .unwrap();
        let d2 = crate::tools::provenance::record_directive_observed(
            &fs,
            DirectiveOrigin::TuiPrompt,
            "second",
            "second",
        )
        .unwrap();
        let attr1 = ProvenanceAttribution::with_directive(d1.event_id.clone());
        crate::tools::requirements::requirements_write(
            &fs,
            crate::tools::requirements::RequirementsWriteArgs {
                upserts: vec![crate::tools::requirements::RequirementInput {
                    id: "r1".to_string(),
                    statement: "one".to_string(),
                }],
                withdraw_ids: vec![],
            },
            &attr1,
        )
        .unwrap();
        // Directive filter returns D + its requirement + linked plan/change/verification.
        let resp = crate::tools::provenance::provenance_read(
            &fs,
            crate::tools::provenance::ProvenanceReadArgs {
                directive_id: Some(d1.event_id.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(
            resp.events.iter().any(|e| e["event_id"] == d1.event_id),
            "must contain D1"
        );
        assert!(
            !resp.events.iter().any(|e| e["event_id"] == d2.event_id),
            "must not contain D2"
        );
        // Requirement filter.
        let resp2 = crate::tools::provenance::provenance_read(
            &fs,
            crate::tools::provenance::ProvenanceReadArgs {
                requirement_id: Some("r1".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(
            resp2
                .events
                .iter()
                .any(|e| e["type"] == "requirement_changed"),
            "must contain requirement_changed"
        );
        // New event types filter.
        let resp3 = crate::tools::provenance::provenance_read(
            &fs,
            crate::tools::provenance::ProvenanceReadArgs {
                event_types: Some(vec![
                    crate::provenance::ProvenanceEventType::DirectiveObserved,
                ]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(
            resp3
                .events
                .iter()
                .all(|e| e["type"] == "directive_observed")
        );
    }

    #[tokio::test]
    async fn test_provenance_read_hides_directive_content_by_default() {
        let proj = tempfile::tempdir().unwrap();
        let fs = fs_with_session(proj.path());
        let secret = "token sk-secret-123";
        let d = crate::tools::provenance::record_directive_observed(
            &fs,
            DirectiveOrigin::TuiPrompt,
            secret,
            secret,
        )
        .unwrap();
        let _ = d;
        let resp = crate::tools::provenance::provenance_read(
            &fs,
            crate::tools::provenance::ProvenanceReadArgs::default(),
        )
        .await
        .unwrap();
        // Default returns preview + hashes, never the full-text fields.
        for e in &resp.events {
            assert!(
                e.get("raw_input").is_none(),
                "raw_input must be hidden by default"
            );
            assert!(
                e.get("effective_instruction").is_none(),
                "effective_instruction must be hidden by default"
            );
        }
        // Hashes are present so identity is still verifiable.
        let serialized = serde_json::to_string(&resp.events).unwrap();
        assert!(serialized.contains("blake3:"));
        let full = crate::tools::provenance::provenance_read(
            &fs,
            crate::tools::provenance::ProvenanceReadArgs {
                include_content: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let serialized_full = serde_json::to_string(&full.events).unwrap();
        assert!(serialized_full.contains("sk-secret-123"));
    }

    #[test]
    fn test_plan_requirement_links_helper() {
        let current = vec![plan_item("p1", "pending", vec!["r1".to_string()])];
        let events = vec![];
        let links = plan_requirement_links(&current, &events);
        assert_eq!(links["p1"], vec!["r1".to_string()]);
    }

    #[test]
    fn test_requirement_changed_event_carries_directive() {
        let e = RequirementChangedEvent {
            directive_id: "d1".to_string(),
            changes: vec![RequirementTransition {
                requirement_id: "r1".to_string(),
                before: None,
                after: Some(RequirementSnapshot {
                    id: "r1".to_string(),
                    statement: "s".to_string(),
                    status: RequirementStatus::Active,
                }),
            }],
        };
        assert_eq!(e.directive_id, "d1");
    }
}
