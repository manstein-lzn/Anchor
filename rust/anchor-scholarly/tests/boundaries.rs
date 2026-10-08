mod support;

use std::{fs, os::unix::fs::symlink, path::Path, process::Command, time::Duration};

use anchor_scholarly::{MAX_RESPONSE_BYTES, SearchRequest, Source, cli::Cli, read_queries};
use rustix::fs::{CWD, Mode, mkfifoat};

use support::{ARXIV_HTML, CROSSREF, Fixture, Reply};

fn request(source: Source) -> SearchRequest {
    SearchRequest {
        query: "test".to_owned(),
        source,
        limit: 8,
        offset: 0,
    }
}

#[tokio::test]
async fn malformed_json_schema_and_redirect_errors_are_not_successes() {
    for (body, source, host) in [
        ("not JSON", Source::Crossref, "api.crossref.org"),
        ("{}", Source::Crossref, "api.crossref.org"),
        (
            "{\"message\":{\"items\":[{}]}}",
            Source::Crossref,
            "api.crossref.org",
        ),
        ("not JSON", Source::Openalex, "api.openalex.org"),
        (
            "{\"error\":\"missing API key\"}",
            Source::Openalex,
            "api.openalex.org",
        ),
        ("{\"results\":[{}]}", Source::Openalex, "api.openalex.org"),
    ] {
        let fixture = Fixture::new().await;
        fixture.route(host, "/works", vec![Reply::body(body)]).await;
        let error = fixture
            .scholarly()
            .search(&request(source), Duration::from_secs(3))
            .await
            .unwrap_err();
        assert_eq!(error.code, "invalid_source_response");
        assert!(!error.retryable);
    }
    let fixture = Fixture::new().await;
    fixture
        .route(
            "api.crossref.org",
            "/works",
            vec![Reply::redirect("https://127.0.0.1/private")],
        )
        .await;
    let error = fixture
        .scholarly()
        .search(&request(Source::Crossref), Duration::from_secs(3))
        .await
        .unwrap_err();
    assert_eq!(error.code, "invalid_request");
    assert_eq!(fixture.requests().await.len(), 1);
}

#[tokio::test]
async fn valid_relative_redirect_is_revalidated_and_reported_as_final_request_url() {
    let fixture = Fixture::new().await;
    fixture
        .route(
            "api.crossref.org",
            "/works",
            vec![Reply::redirect("/next?kept=yes")],
        )
        .await;
    fixture
        .route("api.crossref.org", "/next", vec![Reply::body(CROSSREF)])
        .await;
    let result = fixture
        .scholarly()
        .search(&request(Source::Crossref), Duration::from_secs(3))
        .await
        .unwrap();
    assert_eq!(
        result["request_url"],
        "https://api.crossref.org/next?kept=yes"
    );
    assert_eq!(fixture.requests().await.len(), 2);
    assert_eq!(fixture.transport.pins.lock().await.len(), 2);
}

#[tokio::test]
async fn arxiv_dtd_external_entities_are_rejected_before_safe_web_fallback() {
    let fixture = Fixture::new().await;
    let attack = "<!DOCTYPE feed [<!ENTITY secret SYSTEM 'file:///etc/passwd'>]><feed xmlns='http://www.w3.org/2005/Atom'><entry><title>&secret;</title></entry></feed>";
    fixture
        .route("export.arxiv.org", "/api/query", vec![Reply::body(attack)])
        .await;
    fixture
        .route("arxiv.org", "/search/", vec![Reply::body(ARXIV_HTML)])
        .await;
    let result = fixture
        .scholarly()
        .search(&request(Source::Arxiv), Duration::from_secs(8))
        .await
        .unwrap();
    assert_eq!(result["papers"][0]["evidence_level"], "listing");
    assert!(!serde_json::to_string(&result).unwrap().contains("root:"));
    assert_eq!(fixture.requests().await.len(), 2);
}

#[tokio::test]
async fn declared_and_streamed_response_size_limits_are_both_enforced() {
    for chunked in [false, true] {
        let fixture = Fixture::new().await;
        let mut reply = Reply::body(vec![b' '; MAX_RESPONSE_BYTES + 1]);
        reply.chunked = chunked;
        fixture
            .route("api.crossref.org", "/works", vec![reply])
            .await;
        let error = fixture
            .scholarly()
            .search(&request(Source::Crossref), Duration::from_secs(5))
            .await
            .unwrap_err();
        assert_eq!(error.code, "source_too_large");
    }
}

#[tokio::test]
async fn delayed_headers_and_delayed_body_time_out() {
    for delay_body in [false, true] {
        let fixture = Fixture::new().await;
        let mut reply = Reply::body(CROSSREF);
        if delay_body {
            reply.delay_body = Duration::from_secs(2);
        } else {
            reply.delay_headers = Duration::from_secs(2);
        }
        fixture
            .route("api.crossref.org", "/works", vec![reply])
            .await;
        let error = fixture
            .scholarly()
            .search(&request(Source::Crossref), Duration::from_millis(100))
            .await
            .unwrap_err();
        assert_eq!(error.code, "source_timeout");
        assert!(error.retryable);
    }
}

#[tokio::test]
async fn invalid_search_bounds_never_issue_http_requests() {
    let fixture = Fixture::new().await;
    for (query, limit, offset) in [
        ("".to_owned(), 8, 0),
        ("字".repeat(1001), 8, 0),
        ("q".to_owned(), 0, 0),
        ("q".to_owned(), 101, 0),
        ("q".to_owned(), 8, -1),
    ] {
        let search = SearchRequest {
            query,
            source: Source::Crossref,
            limit,
            offset,
        };
        assert_eq!(
            fixture
                .scholarly()
                .search(&search, Duration::from_secs(1))
                .await
                .unwrap_err()
                .code,
            "invalid_request"
        );
    }
    assert!(fixture.requests().await.is_empty());
}

#[test]
fn query_files_reject_symlinks_traversal_special_files_size_and_invalid_utf8() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("queries.txt");
    fs::write(&file, "one\n # ignore\n\n two \n").unwrap();
    assert_eq!(read_queries(&file).unwrap(), vec!["one", "two"]);
    let link = directory.path().join("link.txt");
    symlink(&file, &link).unwrap();
    assert!(read_queries(&link).is_err());
    let nested = directory.path().join("nested");
    fs::create_dir(&nested).unwrap();
    let linked_directory = directory.path().join("linked-directory");
    symlink(&nested, &linked_directory).unwrap();
    fs::write(nested.join("queries.txt"), "q").unwrap();
    assert!(read_queries(&linked_directory.join("queries.txt")).is_err());
    assert!(read_queries(&nested.join("../queries.txt")).is_err());
    assert!(read_queries(&nested).is_err());
    assert!(read_queries(Path::new("/dev/null")).is_err());
    let fifo = directory.path().join("fifo");
    mkfifoat(CWD, &fifo, Mode::RUSR | Mode::WUSR).unwrap();
    assert!(read_queries(&fifo).is_err());
    fs::write(&file, [0xff, 0xfe]).unwrap();
    assert!(read_queries(&file).unwrap_err().message.contains("UTF-8"));
    fs::write(&file, vec![b'x'; 1_048_577]).unwrap();
    assert!(
        read_queries(&file)
            .unwrap_err()
            .message
            .contains("size limit")
    );
}

#[test]
fn cli_help_and_invalid_flags_have_documented_streams_and_exit_codes() {
    let binary = env!("CARGO_BIN_EXE_anchor-scholarly");
    let output = Command::new(binary).arg("--help").output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let help = String::from_utf8(output.stdout).unwrap();
    for name in [
        "sources",
        "search",
        "search-many",
        "read",
        "read-many",
        "citations",
    ] {
        assert!(help.contains(name));
    }
    for arguments in [
        vec!["search"],
        vec!["search", "--query", "q", "--source", "wrong"],
        vec!["search", "--query", "q", "--unknown"],
        vec![],
    ] {
        let output = Command::new(binary).args(arguments).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
    let output = Command::new(binary)
        .args(["search", "--query", "q", "--limit", "-1"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .starts_with("anchor-scholarly: ValueError:")
    );
}

#[test]
fn read_and_citation_commands_are_exposed_without_fallback() {
    for arguments in [
        vec![
            "anchor-scholarly",
            "read",
            "--url",
            "https://arxiv.org/pdf/2401.00001",
            "--offset",
            "24000",
            "--page-start",
            "40",
        ],
        vec![
            "anchor-scholarly",
            "read-many",
            "--urls",
            "https://a.org,https://b.org",
        ],
        vec![
            "anchor-scholarly",
            "citations",
            "--identifier",
            "doi:10.1234/q",
            "--direction",
            "cites",
        ],
    ] {
        assert!(Cli::try_parse_arguments(arguments).is_ok());
    }
}
