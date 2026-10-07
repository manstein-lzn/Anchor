mod support;

use std::{collections::HashMap, time::Duration};

use anchor_scholarly::{SearchRequest, Source, cli::Cli};
use serde_json::json;

use support::{ARXIV, ARXIV_HTML, CROSSREF, Fixture, OPENALEX, Reply};

fn request(source: Source) -> SearchRequest {
    SearchRequest {
        query: "cost models & 编译器".to_owned(),
        source,
        limit: 8,
        offset: 24,
    }
}

#[tokio::test]
async fn crossref_maps_metadata_and_preserves_legacy_offset_behavior() {
    let fixture = Fixture::new().await;
    fixture
        .route("api.crossref.org", "/works", vec![Reply::body(CROSSREF)])
        .await;
    let result = fixture
        .scholarly()
        .search(&request(Source::Crossref), Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(result["source"], "crossref");
    assert_eq!(result["total_results"], 101);
    assert_eq!(
        result["papers"][0],
        json!({
            "id": "doi:10.1234/cost-model", "doi": "10.1234/cost-model", "title": "Learned cost models",
            "authors": ["Ada Lovelace", "Turing"], "year": 2024, "venue": "Compiler Journal",
            "publication_type": "journal-article", "url": "https://ieeexplore.ieee.org/document/42",
            "abstract": "Cost models & evidence.", "fulltext_urls": ["https://publisher.example.org/paper.pdf"], "evidence_level": "abstract"
        })
    );
    assert_eq!(result["papers"][1]["year"], serde_json::Value::Null);
    assert_eq!(result["papers"][1]["evidence_level"], "metadata_only");
    assert_eq!(result["untrusted_source_content"], true);
    assert!(chrono::DateTime::parse_from_rfc3339(result["retrieved_at"].as_str().unwrap()).is_ok());
    let calls = fixture.requests().await;
    let parameters = calls[0]
        .query_pairs()
        .into_owned()
        .collect::<HashMap<_, _>>();
    assert_eq!(parameters["query.bibliographic"], "cost models & 编译器");
    assert_eq!(parameters["rows"], "8");
    assert!(!parameters.contains_key("offset"));
    assert_eq!(result["request_url"], calls[0].to_string());
    assert_eq!(
        fixture.transport.pins.lock().await[0].to_string(),
        "93.184.216.34:443"
    );
}

#[tokio::test]
async fn openalex_maps_abstract_locations_and_page_based_offsets() {
    let fixture = Fixture::new().await;
    fixture
        .route("api.openalex.org", "/works", vec![Reply::body(OPENALEX)])
        .await;
    let mut search = request(Source::Openalex);
    search.offset = 25;
    let result = fixture
        .scholarly()
        .search(&search, Duration::from_secs(5))
        .await
        .unwrap();
    let paper = &result["papers"][0];
    assert_eq!(paper["id"], "doi:10.1234/cost-model");
    assert_eq!(paper["abstract"], "Learned cost models cost");
    assert_eq!(
        paper["fulltext_urls"],
        json!([
            "https://publisher.example.org/paper.pdf",
            "https://publisher.example.org/paper",
            "https://arxiv.org/abs/2401.00001"
        ])
    );
    assert_eq!(paper["cited_by_count"], 12);
    assert_eq!(paper["venue"], "Compiler Journal");
    assert_eq!(result["papers"][1]["id"], "openalex:W43");
    assert_eq!(result["papers"][1]["doi"], serde_json::Value::Null);
    assert_eq!(result["papers"][1]["evidence_level"], "metadata_only");
    let calls = fixture.requests().await;
    let parameters = calls[0]
        .query_pairs()
        .into_owned()
        .collect::<HashMap<_, _>>();
    assert_eq!(parameters["page"], "4");
    assert_eq!(parameters["per-page"], "8");
    assert_eq!(
        parameters["filter"],
        "title_and_abstract.search:cost models & 编译器"
    );
    assert!(!parameters.contains_key("search"));
}

#[tokio::test]
async fn arxiv_atom_preserves_namespace_metadata_and_legacy_start_zero() {
    let fixture = Fixture::new().await;
    fixture
        .route("export.arxiv.org", "/api/query", vec![Reply::body(ARXIV)])
        .await;
    let result = fixture
        .scholarly()
        .search(&request(Source::Arxiv), Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(
        result["papers"][0],
        json!({
            "id": "https://arxiv.org/abs/2401.00001v2", "url": "https://arxiv.org/abs/2401.00001v2",
            "title": "Learned cost models", "authors": ["Ada Lovelace", "Alan Turing"], "year": "2024",
            "abstract": "Evidence from compiler optimization.", "publication_type": "preprint",
            "evidence_level": "abstract", "fulltext_urls": ["https://arxiv.org/pdf/2401.00001v2"]
        })
    );
    let calls = fixture.requests().await;
    let parameters = calls[0]
        .query_pairs()
        .into_owned()
        .collect::<HashMap<_, _>>();
    assert_eq!(parameters["start"], "0");
    assert_eq!(parameters["max_results"], "8");
    assert_eq!(parameters["sortBy"], "relevance");
    assert_eq!(calls.len(), 1);
    assert!(result.get("total_results").is_none());
}

#[tokio::test]
async fn arxiv_web_fallback_has_exact_class_matching_year_and_valid_page_size() {
    let fixture = Fixture::new().await;
    fixture
        .route("export.arxiv.org", "/api/query", vec![Reply::status(403)])
        .await;
    fixture
        .route("arxiv.org", "/search/", vec![Reply::body(ARXIV_HTML)])
        .await;
    let mut search = request(Source::Arxiv);
    search.limit = 30;
    let result = fixture
        .scholarly()
        .search(&search, Duration::from_secs(8))
        .await
        .unwrap();
    assert_eq!(result["papers"].as_array().unwrap().len(), 1);
    assert_eq!(result["papers"][0]["id"], "arxiv:2401.00001v2");
    assert_eq!(result["papers"][0]["title"], "Learned cost models");
    assert_eq!(result["papers"][0]["year"], "2023");
    assert_eq!(result["papers"][0]["authors"], json!(["Alan Turing"]));
    assert_eq!(result["papers"][0]["evidence_level"], "listing");
    assert!(
        result["note"]
            .as_str()
            .unwrap()
            .contains("API endpoint was unavailable")
    );
    let calls = fixture.requests().await;
    let parameters = calls[1]
        .query_pairs()
        .into_owned()
        .collect::<HashMap<_, _>>();
    assert_eq!(parameters["size"], "50");
    assert_eq!(parameters["order"], "");
    assert_eq!(parameters["searchtype"], "all");
}

#[tokio::test]
async fn cli_search_defaults_and_unicode_roundtrip_use_the_real_http_adapter() {
    let fixture = Fixture::new().await;
    fixture
        .route("api.crossref.org", "/works", vec![Reply::body(CROSSREF)])
        .await;
    let cli =
        Cli::try_parse_arguments(["anchor-scholarly", "search", "--query", "编译器"]).unwrap();
    let result = cli.execute(&fixture.scholarly()).await.unwrap();
    let output = serde_json::to_string(&result).unwrap();
    assert!(output.contains("编译器"));
    let calls = fixture.requests().await;
    let parameters = calls[0]
        .query_pairs()
        .into_owned()
        .collect::<HashMap<_, _>>();
    assert_eq!(parameters["rows"], "8");
    assert_eq!(parameters["query.bibliographic"], "编译器");
}
