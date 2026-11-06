use super::*;

#[test]
fn issue_exports_a_pinned_revision_and_never_substitutes_another_day() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let server = BriefingServer::new(workspace.path(), root.path()).unwrap();
    let report = br#"{"intro":["first issue"]}"#;
    let mut publication = Publication {
        issue_id: "ai-daily-20261031".into(),
        issue_date: "2026-10-31".into(),
        revision: 1,
        window_start: "2026-10-30T08:00:00+08:00".into(),
        cutoff: "2026-10-31T08:00:00+08:00".into(),
        published_at: Utc::now().to_rfc3339(),
        report_sha256: Some(digest(report)),
        pdf_sha256: digest(b"first pdf"),
        imported_baseline: false,
        visual_review: Some(VisualReview {
            approved: true,
            pages_reviewed: vec![1],
            notes: "Original archived visual review.".into(),
        }),
    };
    server
        .archive(
            &publication,
            &[
                ("briefing.pdf", b"first pdf".to_vec()),
                ("report.json", report.to_vec()),
            ],
        )
        .unwrap();
    let archive = root.path().join("data/issues/2026-10-31");
    let original_publication: Value =
        serde_json::from_slice(&fs::read(archive.join("0001/publication.json")).unwrap()).unwrap();
    assert_eq!(
        original_publication,
        serde_json::to_value(&publication).unwrap()
    );
    publication.revision = 2;
    publication.pdf_sha256 = digest(b"revised pdf");
    publication.visual_review.as_mut().unwrap().notes = "Revised publication visual review.".into();
    server
        .archive(
            &publication,
            &[
                ("briefing.pdf", b"revised pdf".to_vec()),
                ("report.json", report.to_vec()),
            ],
        )
        .unwrap();
    let archive_snapshot = || {
        ["0001", "0002"]
            .into_iter()
            .flat_map(|revision| {
                let directory = archive.join(revision);
                ["publication.json", "briefing.pdf", "report.json"]
                    .into_iter()
                    .map(move |name| directory.join(name))
            })
            .map(|path| {
                let bytes = fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let archived_before_export = archive_snapshot();
    let issue = server
        .issue(IssueRequest {
            issue_date: Some("2026-10-31".into()),
            revision: Some(1),
            output_dir: "pinned".into(),
        })
        .unwrap();
    let manifest_path = workspace
        .path()
        .join(issue["publication_path"].as_str().unwrap());
    let manifest_bytes = fs::read(&manifest_path).unwrap();
    let manifest: Value = serde_json::from_slice(&manifest_bytes).unwrap();
    assert_eq!(issue["publication"], original_publication);
    assert_eq!(manifest["publication"], original_publication);
    assert_ne!(
        manifest["publication"]["visual_review"]["notes"],
        publication.visual_review.as_ref().unwrap().notes
    );
    assert_eq!(manifest["pdf_path"], issue["pdf_path"]);
    assert_eq!(manifest["report_path"], issue["report_path"]);
    assert_eq!(
        fs::read(workspace.path().join(issue["pdf_path"].as_str().unwrap())).unwrap(),
        b"first pdf"
    );
    assert_eq!(
        fs::read(
            workspace
                .path()
                .join(issue["report_path"].as_str().unwrap())
        )
        .unwrap(),
        report
    );
    assert_eq!(archive_snapshot(), archived_before_export);
    assert_eq!(
        server
            .issue(IssueRequest {
                issue_date: Some("2026-10-31".into()),
                revision: Some(1),
                output_dir: "pinned".into(),
            })
            .unwrap(),
        issue
    );
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest_bytes);
    assert_eq!(archive_snapshot(), archived_before_export);
    assert!(
        server
            .issue(IssueRequest {
                issue_date: Some("2026-11-01".into()),
                revision: None,
                output_dir: "missing".into()
            })
            .unwrap_err()
            .to_string()
            .contains("not available")
    );
    assert!(!workspace.path().join("missing").exists());

    fs::write(archive.join("0002/report.json"), b"changed report").unwrap();
    let archived_before_failure = archive_snapshot();
    let partial = workspace.path().join("changed-report");
    fs::create_dir(&partial).unwrap();
    fs::write(partial.join("keep.txt"), b"keep export failure evidence").unwrap();
    assert!(
        server
            .issue(IssueRequest {
                issue_date: Some("2026-10-31".into()),
                revision: None,
                output_dir: "changed-report".into(),
            })
            .unwrap_err()
            .to_string()
            .contains("archived report differs from its published identity")
    );
    let partial_pdf = partial.join(
        Path::new(issue["pdf_path"].as_str().unwrap())
            .file_name()
            .unwrap(),
    );
    assert_eq!(fs::read(&partial_pdf).unwrap(), b"revised pdf");
    assert!(!partial.join("report.json").exists());
    assert!(!partial.join("publication.json").exists());
    assert_eq!(archive_snapshot(), archived_before_failure);
    assert!(
        server
            .issue(IssueRequest {
                issue_date: Some("2026-10-31".into()),
                revision: Some(2),
                output_dir: "changed-report".into(),
            })
            .is_err()
    );
    assert_eq!(fs::read(&partial_pdf).unwrap(), b"revised pdf");
    assert_eq!(
        fs::read(partial.join("keep.txt")).unwrap(),
        b"keep export failure evidence"
    );
    assert!(!partial.join("publication.json").exists());
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest_bytes);
    assert_eq!(archive_snapshot(), archived_before_failure);

    fs::write(archive.join("0002/briefing.pdf"), b"changed").unwrap();
    let archived_before_failure = archive_snapshot();
    assert!(
        server
            .issue(IssueRequest {
                issue_date: Some("2026-10-31".into()),
                revision: None,
                output_dir: "changed".into()
            })
            .unwrap_err()
            .to_string()
            .contains("published identity")
    );
    assert!(!workspace.path().join("changed").exists());
    assert_eq!(archive_snapshot(), archived_before_failure);
}

#[test]
fn context_preserves_same_day_window_and_previous_issue_for_deduplication() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let server = BriefingServer::new(workspace.path(), root.path()).unwrap();
    let date = Utc::now()
        .with_timezone(&FixedOffset::east_opt(28800).unwrap())
        .format("%Y-%m-%d")
        .to_string();
    let publication = Publication {
        issue_id: format!("ai-daily-{}", date.replace('-', "")),
        issue_date: date.clone(),
        revision: 2,
        window_start: "2026-10-30T16:07:00+08:00".into(),
        cutoff: "2026-10-31T08:00:00+08:00".into(),
        published_at: Utc::now().to_rfc3339(),
        report_sha256: None,
        pdf_sha256: digest(b"baseline"),
        imported_baseline: true,
        visual_review: None,
    };
    let prior_date = chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d")
        .unwrap()
        .pred_opt()
        .unwrap()
        .to_string();
    let prior = Publication {
        issue_id: format!("ai-daily-{}", prior_date.replace('-', "")),
        issue_date: prior_date,
        revision: 1,
        cutoff: "2026-10-30T16:07:00+08:00".into(),
        ..publication.clone()
    };
    server
        .archive(
            &prior,
            &[("briefing.txt", b"already covered yesterday".to_vec())],
        )
        .unwrap();
    server
        .archive(
            &publication,
            &[
                ("briefing.pdf", b"baseline".to_vec()),
                ("briefing.txt", b"original news".to_vec()),
            ],
        )
        .unwrap();
    let context = server
        .context(ContextRequest {
            output_dir: "context".into(),
        })
        .unwrap();
    assert_eq!(context["issue_window_start"], publication.window_start);
    assert_eq!(context["search_window_start"], publication.cutoff);
    assert_eq!(context["previous"]["publication"]["revision"], 2);
    assert_eq!(
        context["previous_issue"]["publication"]["issue_date"],
        prior.issue_date
    );
    assert_eq!(
        fs::read(
            workspace
                .path()
                .join(context["previous_issue"]["text_path"].as_str().unwrap())
        )
        .unwrap(),
        b"already covered yesterday"
    );
    assert_eq!(
        fs::read(
            workspace
                .path()
                .join(context["previous"]["text_path"].as_str().unwrap())
        )
        .unwrap(),
        b"original news"
    );
    assert!(
        server
            .context(ContextRequest {
                output_dir: "../escape".into()
            })
            .is_err()
    );
    assert!(
        server
            .context(ContextRequest {
                output_dir: "context".into()
            })
            .is_err()
    );
}

#[test]
fn publication_rejects_an_inspection_for_different_report_without_advancing_history() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let server = BriefingServer::new(workspace.path(), root.path()).unwrap();
    fs::write(workspace.path().join("report.json"), json!({"issue_date":"2026-10-31","window_start":"2026-10-30T16:07:00+08:00","cutoff":"2026-10-31T08:00:00+08:00","intro":[],"sections":[],"coverage_note":"No verified update."}).to_string()).unwrap();
    fs::write(workspace.path().join("briefing.pdf"), b"test PDF").unwrap();
    fs::write(workspace.path().join("inspection.json"), json!({"static_checks_passed":true,"report_sha256":"old report","pdf_sha256":"old PDF","pages":1}).to_string()).unwrap();
    let result = server.publish(PublishRequest {
        report_path: "report.json".into(),
        pdf_path: "briefing.pdf".into(),
        inspection_path: "inspection.json".into(),
        export_dir: publication_dir(),
        visual_review: VisualReview {
            approved: true,
            pages_reviewed: vec![1],
            notes: "Reviewed".into(),
        },
    });
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("match this exact report")
    );
    assert!(server.publications().unwrap().is_empty());
}

#[test]
#[ignore = "requires ANCHOR_BRIEFING_TEST_TYPST and system Poppler tools"]
fn publishes_checked_pdfs_with_stable_daily_identity_and_retry_without_moving_cutoff()
-> Result<(), Error> {
    let root = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    fs::create_dir_all(root.path().join("bin"))?;
    fs::create_dir_all(root.path().join("templates"))?;
    fs::copy(
        std::env::var_os("ANCHOR_BRIEFING_TEST_TYPST").ok_or("set test Typst binary")?,
        root.path().join("bin/typst"),
    )?;
    fs::write(
        root.path().join("templates/morning.typ"),
        include_bytes!("../../../plugins/ai-morning-briefing/templates/morning.typ"),
    )?;
    let server = BriefingServer::new(workspace.path(), root.path())?;
    let today = Utc::now()
        .with_timezone(&FixedOffset::east_opt(28800).unwrap())
        .date_naive();
    let day_one = today - chrono::Duration::days(2);
    let day_two = today - chrono::Duration::days(1);
    let make = |name: &str, day: NaiveDate, minute: u8| -> Result<PublishRequest, Error> {
        let report_path = format!("{name}.json");
        fs::write(
            workspace.path().join(&report_path),
            serde_json::to_vec(&json!({
                "issue_date":day.to_string(), "window_start":format!("{}T08:00:00+08:00",day),
                "cutoff":format!("{}T09:{minute:02}:00+08:00",day), "intro":[], "sections":[],
                "coverage_note":format!("中文发布契约验收 {name}，内容不作为新闻交付。")
            }))?,
        )?;
        let delivery = server.render(RenderRequest {
            report_path: report_path.clone(),
            output_dir: format!("out-{name}"),
        })?;
        Ok(PublishRequest {
            report_path,
            pdf_path: delivery["pdf_path"].as_str().ok_or("no PDF")?.into(),
            inspection_path: delivery["inspection_path"]
                .as_str()
                .ok_or("no inspection")?
                .into(),
            export_dir: format!("published-{name}"),
            visual_review: VisualReview {
                approved: true,
                pages_reviewed: vec![1],
                notes: "Fixture page reviewed for publication contract.".into(),
            },
        })
    };
    let mut first = make("first", day_one, 0)?;
    first.export_dir = "published".into();
    let original_notes = first.visual_review.notes.clone();
    let mut default_request = serde_json::to_value(&first)?;
    default_request
        .as_object_mut()
        .unwrap()
        .remove("export_dir");
    let published = server.publish(serde_json::from_value(default_request)?)?;
    assert_eq!(published["publication_path"], "published/publication.json");
    let revised = server.publish(make("revised", day_one, 1)?)?;
    assert_eq!(
        published["publication"]["issue_id"],
        revised["publication"]["issue_id"]
    );
    assert_eq!(revised["publication"]["revision"], 2);
    let next = server.publish(make("next", day_two, 0)?)?;
    assert_ne!(
        next["publication"]["issue_id"],
        revised["publication"]["issue_id"]
    );
    assert_eq!(next["publication"]["revision"], 1);
    let archived_publication_path = root
        .path()
        .join("data/issues")
        .join(day_one.to_string())
        .join("0001/publication.json");
    let archived_publication_bytes = fs::read(&archived_publication_path)?;
    first.visual_review.notes = "Additional retry observations are not a new publication.".into();
    let retry = server.publish(first)?;
    assert_eq!(retry["changed"], false);
    assert_eq!(retry["publication"], published["publication"]);
    assert_eq!(retry["publication_path"], published["publication_path"]);
    assert!(retry.get("export_error").is_none());
    assert_eq!(
        fs::read(&archived_publication_path)?,
        archived_publication_bytes
    );
    assert_eq!(server.publications()?.len(), 3);
    let issue = server.issue(IssueRequest {
        issue_date: Some(day_one.to_string()),
        revision: Some(1),
        output_dir: "export-original".into(),
    })?;
    let manifest: Value = serde_json::from_slice(&fs::read(
        workspace
            .path()
            .join(issue["publication_path"].as_str().ok_or("no manifest")?),
    )?)?;
    let archived_publication: Value = serde_json::from_slice(&archived_publication_bytes)?;
    assert_eq!(issue["publication"], published["publication"]);
    assert_eq!(manifest["publication"], archived_publication);
    assert_eq!(
        manifest["publication"]["visual_review"]["notes"],
        original_notes
    );
    let stale = make("stale", day_one, 2)?;
    assert!(
        server
            .publish(stale)
            .unwrap_err()
            .to_string()
            .contains("newer cutoff")
    );
    let mut blocked = make("blocked", day_two, 2)?;
    let blocked_directory = workspace.path().join(&blocked.export_dir);
    fs::create_dir(&blocked_directory)?;
    let blocked_pdf =
        blocked_directory.join(format!("AI每日晨报_{}.pdf", day_two.format("%Y%m%d")));
    fs::write(&blocked_pdf, b"keep partial export evidence")?;
    let accepted = server.publish(serde_json::from_value(serde_json::to_value(&blocked)?)?)?;
    assert_eq!(accepted["changed"], true);
    assert_eq!(accepted["publication"]["revision"], 2);
    assert!(accepted["export_error"].is_string());
    assert!(accepted.get("publication_path").is_none());
    assert_eq!(server.publications()?.len(), 4);
    blocked.visual_review.notes = "A retry cannot rewrite accepted review notes.".into();
    let blocked_retry = server.publish(serde_json::from_value(serde_json::to_value(&blocked)?)?)?;
    assert_eq!(blocked_retry["changed"], false);
    assert_eq!(blocked_retry["publication"], accepted["publication"]);
    assert!(blocked_retry["export_error"].is_string());
    assert!(!blocked_directory.join("publication.json").exists());
    assert_eq!(fs::read(&blocked_pdf)?, b"keep partial export evidence");
    blocked.export_dir = "recovered-export".into();
    let recovered = server.publish(blocked)?;
    assert_eq!(recovered["changed"], false);
    assert_eq!(recovered["publication"], accepted["publication"]);
    assert!(recovered.get("export_error").is_none());
    assert_eq!(server.publications()?.len(), 4);
    let context = server.context(ContextRequest {
        output_dir: "context".into(),
    })?;
    assert_eq!(context["previous"]["publication"], recovered["publication"]);
    assert_eq!(
        context["search_window_start"],
        recovered["publication"]["cutoff"]
    );
    Ok(())
}
