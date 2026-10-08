mod support;

use std::time::Duration;

use anchor_scholarly::{CitationRequest, Direction, ReadManyRequest, ReadRequest};
use axum::http::{HeaderValue, header::CONTENT_TYPE};
use lopdf::{
    Document, Object, Stream,
    content::{Content, Operation},
    dictionary,
};
use serde_json::json;

use support::{Fixture, Reply};

fn text_document(text: &str) -> Reply {
    let mut reply = Reply::body(text);
    reply
        .headers
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    reply
}

fn pdf_document(page_count: usize) -> Vec<u8> {
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
    });
    let resources = document.add_object(dictionary! {"Font" => dictionary! {"F1" => font_id}});
    let mut pages = Vec::new();
    for index in 0..page_count {
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 10.into()]),
                Operation::new("Td", vec![20.into(), 700.into()]),
                Operation::new(
                    "Tj",
                    vec![Object::string_literal(format!(
                        "page-{index:02} Deterministic scholarly PDF extraction preserves the requested page window and uses an established parser."
                    ))],
                ),
                Operation::new("ET", vec![]),
            ],
        };
        let stream = document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page = document.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => stream,
        });
        pages.push(Object::Reference(page));
    }
    document.objects.insert(pages_id, dictionary! {
        "Type" => "Pages", "Kids" => pages, "Count" => page_count as i64,
        "Resources" => resources, "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    }.into());
    let catalog = document.add_object(dictionary! {"Type" => "Catalog", "Pages" => pages_id});
    let info = document.add_object(dictionary! {"Title" => Object::string_literal("Fixture PDF")});
    document.trailer.set("Root", catalog);
    document.trailer.set("Info", info);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

#[tokio::test]
async fn html_readability_extracts_article_title_and_text() {
    let fixture = Fixture::new().await;
    let paragraph = "This scholarly article documents measured compiler optimization results with reproducible experiments and carefully described evidence. ".repeat(20);
    let mut reply = Reply::body(format!(
        "<!DOCTYPE html><html><head><title>Fixture article</title></head><body><nav>unrelated navigation</nav><main><article><h1>Fixture article</h1><p>{paragraph}</p></article></main></body></html>"
    ));
    reply.headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    fixture
        .route("papers.example.org", "/article", vec![reply])
        .await;
    let result = fixture
        .scholarly()
        .read(
            &ReadRequest {
                url: "https://papers.example.org/article".into(),
                offset: 0,
                page_start: 0,
            },
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(result["title"], "Fixture article");
    assert!(
        result["text"]
            .as_str()
            .unwrap()
            .contains("measured compiler optimization")
    );
    assert!(
        !result["text"]
            .as_str()
            .unwrap()
            .contains("unrelated navigation")
    );
}

#[tokio::test]
async fn pdf_extraction_uses_forty_page_windows_and_rejects_corruption() {
    let fixture = Fixture::new().await;
    let bytes = pdf_document(41);
    let mut first = Reply::body(&bytes);
    first
        .headers
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/pdf"));
    let mut second = Reply::body(&bytes);
    second.headers = first.headers.clone();
    fixture
        .route(
            "papers.example.org",
            "/paper.pdf",
            vec![first, second, Reply::body(b"%PDF-1.5 corrupt")],
        )
        .await;
    let scholarly = fixture.scholarly();
    let request = ReadRequest {
        url: "https://papers.example.org/paper.pdf".into(),
        offset: 0,
        page_start: 0,
    };
    let first = scholarly
        .read(&request, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(first["pages"], 41);
    assert_eq!(first["title"], "Fixture PDF");
    assert_eq!(first["next_page_start"], 40);
    assert!(first["text"].as_str().unwrap().contains("page-00"));
    assert!(first["text"].as_str().unwrap().contains("page-39"));
    assert!(!first["text"].as_str().unwrap().contains("page-40"));
    let last = scholarly
        .read(
            &ReadRequest {
                page_start: 40,
                ..request.clone()
            },
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert!(last["text"].as_str().unwrap().contains("page-40"));
    assert!(!last["text"].as_str().unwrap().contains("page-39"));
    assert!(last.get("next_page_start").is_none());
    let failure = scholarly
        .read(&request, Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(matches!(failure.code, "invalid_pdf" | "unsupported_pdf"));
}

#[tokio::test]
async fn unicode_character_window_and_invalid_offsets_do_not_split_text() {
    let fixture = Fixture::new().await;
    let text = "\u{4e2d}".repeat(24_010);
    fixture
        .route(
            "papers.example.org",
            "/unicode",
            vec![text_document(&text), text_document(&text)],
        )
        .await;
    let scholarly = fixture.scholarly();
    let request = ReadRequest {
        url: "https://papers.example.org/unicode".into(),
        offset: 0,
        page_start: 0,
    };
    let result = scholarly
        .read(&request, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(result["text"].as_str().unwrap().chars().count(), 24_000);
    assert_eq!(result["next_offset"], 24_000);
    assert_eq!(result["truncated"], true);
    let failure = scholarly
        .read(
            &ReadRequest {
                offset: 24_010,
                ..request
            },
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();
    assert_eq!(failure.code, "invalid_request");
}

#[tokio::test]
async fn read_extracts_plain_text_and_preserves_offsets() {
    let fixture = Fixture::new().await;
    let body =
        "A bounded scholarly document with enough text to pass the article extraction threshold. "
            .repeat(3);
    fixture
        .route(
            "papers.example.org",
            "/paper.txt",
            vec![text_document(&body)],
        )
        .await;
    let result = fixture
        .scholarly()
        .read(
            &ReadRequest {
                url: "https://papers.example.org/paper.txt".into(),
                offset: 10,
                page_start: 0,
            },
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(
        result["requested_url"],
        "https://papers.example.org/paper.txt"
    );
    assert_eq!(result["offset"], 10);
    assert_eq!(result["page_start"], 0);
    assert!(result["text"].as_str().unwrap().starts_with("scholarly"));
    assert_eq!(result["content_type"], "text/plain");
}

#[tokio::test]
async fn read_many_preserves_input_order_duplicates_and_item_errors() {
    let fixture = Fixture::new().await;
    let body = "This paper is long enough for the bounded reader to return a useful deterministic text result. ".repeat(3);
    fixture
        .route(
            "a.example.org",
            "/one",
            vec![text_document(&body), text_document(&body)],
        )
        .await;
    fixture
        .route("b.example.org", "/two", vec![Reply::status(404)])
        .await;
    let urls = vec![
        "https://a.example.org/one".into(),
        "https://b.example.org/two".into(),
        "https://a.example.org/one".into(),
    ];
    let result = fixture
        .scholarly()
        .read_many(
            &ReadManyRequest {
                urls: urls.clone(),
                offset: 0,
                page_start: 0,
            },
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(result["requested_urls"], json!(urls));
    assert_eq!(result["documents"].as_array().unwrap().len(), 3);
    assert!(
        result["documents"][0]["text"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
    );
    assert!(
        result["documents"][1]["error"]
            .as_str()
            .unwrap()
            .contains("HTTP 404")
    );
    assert!(
        result["documents"][2]["text"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
    );
    assert_eq!(result["retrieved"], 2);
}

#[tokio::test]
async fn citations_maps_openalex_seed_and_cited_by_results() {
    let fixture = Fixture::new().await;
    fixture
        .route(
            "api.openalex.org",
            "/works/doi:10.1234/seed",
            vec![Reply::body(
                r#"{"id":"https://openalex.org/W1","referenced_works":["https://openalex.org/W2"]}"#,
            )],
        )
        .await;
    fixture
        .route(
            "api.openalex.org",
            "/works",
            vec![Reply::body(
                r#"{"meta":{"count":1},"results":[{"id":"https://openalex.org/W9","title":"Citing work","publication_year":2025,"authorships":[],"locations":[],"abstract_inverted_index":null}]}"#,
            )],
        )
        .await;
    let result = fixture
        .scholarly()
        .citations(
            &CitationRequest {
                identifier: "doi:10.1234/seed".into(),
                direction: Direction::CitedBy,
                limit: 4,
            },
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(result["direction"], "cited_by");
    assert_eq!(result["papers"][0]["id"], "openalex:W9");
    assert_eq!(result["total_results"], 1);
    let requests = fixture.requests().await;
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .query_pairs()
            .any(|(key, value)| key == "filter" && value == "cites:W1")
    );
}

#[tokio::test]
async fn citations_fetches_reference_chunks_and_applies_result_limit() {
    let fixture = Fixture::new().await;
    let references = (1..=51)
        .map(|index| format!("https://openalex.org/W{index}"))
        .collect::<Vec<_>>();
    fixture
        .route(
            "api.openalex.org",
            "/works/W99",
            vec![Reply::body(
                json!({"id":"https://openalex.org/W99","referenced_works":references}).to_string(),
            )],
        )
        .await;
    fixture
        .route(
            "api.openalex.org",
            "/works",
            vec![
                Reply::body(r#"{"results":[{"id":"https://openalex.org/W1","title":"First"},{"id":"https://openalex.org/W2","title":"Second"}]}"#),
                Reply::body(r#"{"results":[{"id":"https://openalex.org/W51","title":"Last"}]}"#),
            ],
        )
        .await;
    let result = fixture
        .scholarly()
        .citations(
            &CitationRequest {
                identifier: "openalex:W99".into(),
                direction: Direction::Cites,
                limit: 2,
            },
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(result["direction"], "cites");
    assert_eq!(result["total_results"], 51);
    assert_eq!(result["papers"].as_array().unwrap().len(), 2);
    assert_eq!(result["papers"][0]["id"], "openalex:W1");
    let requests = fixture.requests().await;
    assert_eq!(requests.len(), 3);
    let filters = requests[1..]
        .iter()
        .map(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "filter")
                .unwrap()
                .1
                .into_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        filters[0],
        format!(
            "openalex_id:{}",
            (1..=50)
                .map(|index| format!("W{index}"))
                .collect::<Vec<_>>()
                .join("|")
        )
    );
    assert_eq!(filters[1], "openalex_id:W51");
}

#[tokio::test]
async fn arxiv_prefers_html_and_falls_back_to_pdf_without_fetching_abstracts() {
    let fixture = Fixture::new().await;
    let paragraph = "Deterministic arXiv article extraction uses measured evidence and preserves reproducible methodology. ".repeat(20);
    let mut html = Reply::body(format!(
        "<!DOCTYPE html><html><head><title>arXiv fixture</title></head><body><article><h1>arXiv fixture</h1><p>{paragraph}</p></article></body></html>"
    ));
    html.headers
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/html"));
    fixture
        .route(
            "arxiv.org",
            "/html/2401.00001",
            vec![html, Reply::status(404)],
        )
        .await;
    let mut pdf = Reply::body(pdf_document(1));
    pdf.headers
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/pdf"));
    fixture
        .route("arxiv.org", "/pdf/2401.00001.pdf", vec![pdf])
        .await;
    let scholarly = fixture.scholarly();
    let request = ReadRequest {
        url: "https://arxiv.org/pdf/2401.00001.pdf".into(),
        offset: 0,
        page_start: 0,
    };
    let html = scholarly
        .read(&request, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(html["requested_url"], request.url);
    assert_eq!(html["url"], "https://arxiv.org/html/2401.00001");
    assert_eq!(html["title"], "arXiv fixture");
    assert_eq!(fixture.requests().await.len(), 1);
    let pdf = scholarly
        .read(&request, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(pdf["pages"], 1);
    assert!(pdf["text"].as_str().unwrap().contains("page-00"));
    let requests = fixture.requests().await;
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].path(), "/html/2401.00001");
    assert_eq!(requests[2].path(), "/pdf/2401.00001.pdf");
    let error = scholarly
        .read(
            &ReadRequest {
                url: "https://arxiv.org/abs/2401.00001".into(),
                ..request
            },
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "abstract_page");
    assert_eq!(fixture.requests().await.len(), 3);
}

#[tokio::test]
async fn arxiv_html_and_pdf_fallback_share_one_io_deadline() {
    let fixture = Fixture::new().await;
    let mut html = Reply::status(404);
    html.delay_headers = Duration::from_millis(500);
    fixture
        .route("arxiv.org", "/html/2401.00002", vec![html])
        .await;
    fixture
        .route(
            "arxiv.org",
            "/pdf/2401.00002",
            vec![text_document(
                &"The fallback must not renew the exhausted network deadline. ".repeat(3),
            )],
        )
        .await;
    let error = fixture
        .scholarly()
        .read(
            &ReadRequest {
                url: "https://arxiv.org/pdf/2401.00002".into(),
                offset: 0,
                page_start: 0,
            },
            Duration::from_millis(100),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "source_timeout");
    let requests = fixture.requests().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path(), "/html/2401.00002");
}
