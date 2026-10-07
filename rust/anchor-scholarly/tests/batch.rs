mod support;

use std::{fs, time::Duration};

use anchor_scholarly::{Source, cli::Cli};
use reqwest::header::{HeaderValue, RETRY_AFTER};
use serde_json::json;

use support::{ARXIV, CROSSREF, Fixture, OPENALEX, Reply};

#[tokio::test]
async fn sources_reports_each_probe_in_legacy_order_without_untrusted_flag() {
    let fixture = Fixture::new().await;
    fixture
        .route("api.crossref.org", "/works", vec![Reply::body(CROSSREF)])
        .await;
    fixture
        .route("api.openalex.org", "/works", vec![Reply::status(403)])
        .await;
    fixture
        .route("export.arxiv.org", "/api/query", vec![Reply::body(ARXIV)])
        .await;
    let cli = Cli::try_parse_arguments(["anchor-scholarly", "sources"]).unwrap();
    let result = cli.execute(&fixture.scholarly()).await.unwrap();
    assert_eq!(result["usable"], json!(["crossref", "arxiv"]));
    assert_eq!(result["sources"][0]["source"], "crossref");
    assert_eq!(result["sources"][1]["source"], "openalex");
    assert_eq!(result["sources"][1]["usable"], false);
    assert!(
        result["sources"][1]["detail"]
            .as_str()
            .unwrap()
            .contains("HTTP 403")
    );
    assert!(result.get("untrusted_source_content").is_none());
    for call in fixture.requests().await {
        assert!(call.query_pairs().any(|(name, value)| (name == "rows"
            || name == "per-page"
            || name == "max_results")
            && value == "1"));
    }
}

#[tokio::test]
async fn sources_all_down_is_an_honest_successful_status_report() {
    let fixture = Fixture::new().await;
    for (host, path) in [
        ("api.crossref.org", "/works"),
        ("api.openalex.org", "/works"),
        ("export.arxiv.org", "/api/query"),
        ("arxiv.org", "/search/"),
    ] {
        fixture.route(host, path, vec![Reply::status(403)]).await;
    }
    let result = fixture.scholarly().sources(Duration::from_secs(8)).await;
    assert_eq!(result["usable"], json!([]));
    assert!(
        result["note"]
            .as_str()
            .unwrap()
            .starts_with("none answered")
    );
    assert!(
        result["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|source| source["usable"] == false)
    );
}

#[tokio::test]
async fn batch_keeps_duplicates_and_order_and_isolates_a_failed_query() {
    let fixture = Fixture::new().await;
    fixture
        .route(
            "api.crossref.org",
            "/works",
            vec![
                Reply::body(CROSSREF),
                Reply::status(404),
                Reply::body(CROSSREF),
            ],
        )
        .await;
    let result = fixture
        .scholarly()
        .search_many(
            vec![" same ".into(), "bad".into(), "same".into()],
            Source::Crossref,
            8,
            0,
            420,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(result["queries"], json!(["same", "bad", "same"]));
    assert_eq!(result["results"][1]["papers"], json!([]));
    assert!(
        result["results"][1]["error"]
            .as_str()
            .unwrap()
            .contains("ResearchToolError: source returned HTTP 404")
    );
    assert_eq!(result["attempted"], 3);
    assert_eq!(result["with_results"], 2);
    assert_eq!(result["not_attempted"], json!([]));
    assert_eq!(result["ran_out_of_time"], false);
    assert_eq!(result["untrusted_source_content"], true);
    assert!(result["results"][0].get("retrieved_at").is_none());
}

#[tokio::test]
async fn batch_budget_returns_an_inflight_timeout_and_names_the_remaining_queries() {
    let fixture = Fixture::new().await;
    let mut reply = Reply::body(CROSSREF);
    reply.delay_headers = Duration::from_secs(2);
    fixture
        .route("api.crossref.org", "/works", vec![reply])
        .await;
    let result = fixture
        .scholarly()
        .search_many(
            vec!["slow".into(), "not reached".into()],
            Source::Crossref,
            8,
            0,
            1,
            Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(result["attempted"], 1);
    assert_eq!(result["not_attempted"], json!(["not reached"]));
    assert_eq!(result["ran_out_of_time"], true);
    assert_eq!(result["with_results"], 0);
    assert!(
        result["results"][0]["error"]
            .as_str()
            .unwrap()
            .contains("timed out")
    );
}

#[tokio::test]
async fn cli_queries_file_strips_blank_lines_and_comments_and_forwards_options() {
    let fixture = Fixture::new().await;
    fixture
        .route("api.openalex.org", "/works", vec![Reply::body(OPENALEX)])
        .await;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("queries.txt");
    fs::write(&path, "\n # comment\n 编译器 \n\n").unwrap();
    let cli = Cli::try_parse_arguments([
        "anchor-scholarly",
        "search-many",
        "--queries-file",
        path.to_str().unwrap(),
        "--source",
        "openalex",
        "--limit",
        "100",
        "--offset",
        "201",
        "--budget",
        "0",
    ])
    .unwrap();
    let result = cli.execute(&fixture.scholarly()).await.unwrap();
    assert_eq!(result["queries"], json!(["编译器"]));
    assert_eq!(result["attempted"], 1);
    let calls = fixture.requests().await;
    assert!(
        calls[0]
            .query_pairs()
            .any(|(name, value)| name == "page" && value == "3")
    );
    assert!(
        calls[0]
            .query_pairs()
            .any(|(name, value)| name == "per-page" && value == "100")
    );
}

#[tokio::test]
async fn batch_rejects_global_limits_but_long_individual_queries_are_results() {
    let fixture = Fixture::new().await;
    let scholarly = fixture.scholarly();
    for (queries, limit, offset, budget) in [
        (vec!["q".into(); 41], 8, 0, 420),
        (Vec::new(), 8, 0, 420),
        (vec![" ".into()], 8, 0, 420),
        (vec!["q".into()], 101, 0, 420),
        (vec!["q".into()], 8, -1, 420),
        (vec!["q".into()], 8, 0, -1),
        (vec!["q".into()], 8, 0, 3601),
    ] {
        assert!(
            scholarly
                .search_many(
                    queries,
                    Source::Crossref,
                    limit,
                    offset,
                    budget,
                    Duration::from_secs(5)
                )
                .await
                .is_err()
        );
    }
    let result = scholarly
        .search_many(
            vec!["q".repeat(1001)],
            Source::Crossref,
            8,
            0,
            420,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(result["attempted"], 1);
    assert!(
        result["results"][0]["error"]
            .as_str()
            .unwrap()
            .contains("1000")
    );
    assert!(fixture.requests().await.is_empty());
}

#[tokio::test]
async fn rate_limited_batch_returns_failures_without_hammering_the_source() {
    let fixture = Fixture::new().await;
    let replies = (0..2)
        .map(|_| {
            let mut reply = Reply::status(429);
            reply
                .headers
                .insert(RETRY_AFTER, HeaderValue::from_static("0"));
            reply
        })
        .collect();
    fixture.route("api.crossref.org", "/works", replies).await;
    let result = fixture
        .scholarly()
        .search_many(
            vec!["first".into(), "blocked".into()],
            Source::Crossref,
            8,
            0,
            420,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(result["attempted"], 2);
    assert_eq!(result["with_results"], 0);
    assert!(
        result["results"][0]["error"]
            .as_str()
            .unwrap()
            .contains("HTTP 429")
    );
    assert!(
        result["results"][1]["error"]
            .as_str()
            .unwrap()
            .contains("not available right now")
    );
    assert_eq!(fixture.requests().await.len(), 2);
}
