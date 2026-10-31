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
        visual_review: None,
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
    publication.revision = 2;
    publication.pdf_sha256 = digest(b"revised pdf");
    server
        .archive(
            &publication,
            &[
                ("briefing.pdf", b"revised pdf".to_vec()),
                ("report.json", report.to_vec()),
            ],
        )
        .unwrap();
    let issue = server
        .issue(IssueRequest {
            issue_date: Some("2026-10-31".into()),
            revision: Some(1),
            output_dir: "pinned".into(),
        })
        .unwrap();
    assert_eq!(issue["publication"]["revision"], 1);
    assert_eq!(
        fs::read(workspace.path().join(issue["pdf_path"].as_str().unwrap())).unwrap(),
        b"first pdf"
    );
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
    fs::write(
        root.path().join("data/issues/2026-10-31/0002/briefing.pdf"),
        b"changed",
    )
    .unwrap();
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
            visual_review: VisualReview {
                approved: true,
                pages_reviewed: vec![1],
                notes: "Fixture page reviewed for publication contract.".into(),
            },
        })
    };
    let first = make("first", day_one, 0)?;
    let published = server.publish(serde_json::from_value(serde_json::to_value(&first)?)?)?;
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
    let retry = server.publish(first)?;
    assert_eq!(retry["changed"], false);
    assert_eq!(retry["publication"], published["publication"]);
    let stale = make("stale", day_one, 2)?;
    assert!(
        server
            .publish(stale)
            .unwrap_err()
            .to_string()
            .contains("newer cutoff")
    );
    let context = server.context(ContextRequest {
        output_dir: "context".into(),
    })?;
    assert_eq!(context["previous"]["publication"], next["publication"]);
    assert_eq!(
        context["search_window_start"],
        next["publication"]["cutoff"]
    );
    Ok(())
}
