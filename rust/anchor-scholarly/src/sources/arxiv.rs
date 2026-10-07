use std::time::Duration;

use roxmltree::{Document, Node, ParsingOptions};
use scraper::{Html, Selector};
use serde_json::{Value, json};
use tokio::time::Instant;

use crate::{Error, SearchRequest, fetch::Fetcher};

use super::{compact, endpoint};

const ATOM: &str = "http://www.w3.org/2005/Atom";

pub(super) async fn search(
    fetcher: &Fetcher,
    request: &SearchRequest,
    timeout: Duration,
) -> Result<Value, Error> {
    let started = Instant::now();
    let url = endpoint(
        "https://export.arxiv.org/api/query",
        &[
            ("search_query", request.query.clone()),
            ("start", "0".to_owned()),
            ("max_results", request.limit.to_string()),
            ("sortBy", "relevance".to_owned()),
        ],
    )?;
    if let Ok(document) = fetcher.fetch(url.as_str(), timeout).await
        && let Ok(papers) = atom_papers(&document.body)
    {
        return Ok(
            json!({"source": "arxiv", "query": request.query, "request_url": document.final_url, "papers": papers}),
        );
    }
    let remaining = timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Err(Error::timeout());
    }
    web_search(fetcher, request, remaining).await
}

fn child_text<'document>(node: Node<'document, 'document>, name: &str) -> &'document str {
    node.children()
        .find(|child| child.has_tag_name((ATOM, name)))
        .and_then(|child| child.text())
        .unwrap_or("")
}

fn atom_papers(body: &[u8]) -> Result<Vec<Value>, Error> {
    let text =
        std::str::from_utf8(body).map_err(|_| Error::malformed("invalid arXiv XML encoding"))?;
    let document = Document::parse_with_options(
        text,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 100_000,
        },
    )
    .map_err(|_| Error::malformed("invalid arXiv Atom response"))?;
    if !document.root_element().has_tag_name((ATOM, "feed")) {
        return Err(Error::malformed("arXiv response is not an Atom feed"));
    }
    let mut papers = Vec::new();
    for entry in document
        .root_element()
        .children()
        .filter(|node| node.has_tag_name((ATOM, "entry")))
    {
        let url = child_text(entry, "id").replacen("http://", "https://", 1);
        let authors = entry
            .children()
            .filter(|node| node.has_tag_name((ATOM, "author")))
            .map(|author| child_text(author, "name"))
            .collect::<Vec<_>>();
        let fulltext_urls = entry
            .children()
            .filter(|node| {
                node.has_tag_name((ATOM, "link")) && node.attribute("title") == Some("pdf")
            })
            .filter_map(|node| node.attribute("href"))
            .map(|value| value.replacen("http://", "https://", 1))
            .collect::<Vec<_>>();
        papers.push(json!({"id": url, "url": url, "title": compact(child_text(entry, "title")),
            "authors": authors, "year": child_text(entry, "published").chars().take(4).collect::<String>(),
            "abstract": compact(child_text(entry, "summary")), "publication_type": "preprint",
            "evidence_level": "abstract", "fulltext_urls": fulltext_urls}));
    }
    Ok(papers)
}

async fn web_search(
    fetcher: &Fetcher,
    request: &SearchRequest,
    timeout: Duration,
) -> Result<Value, Error> {
    let size = [25, 50, 100, 200]
        .into_iter()
        .find(|size| *size >= request.limit)
        .unwrap_or(200);
    let url = endpoint(
        "https://arxiv.org/search/",
        &[
            ("searchtype", "all".to_owned()),
            ("query", request.query.clone()),
            ("size", size.to_string()),
            ("order", String::new()),
        ],
    )?;
    let document = fetcher.fetch(url.as_str(), timeout).await?;
    let text = String::from_utf8_lossy(&document.body);
    let tree = Html::parse_document(&text);
    let listing_selector = Selector::parse("li.arxiv-result").expect("constant selector");
    let id_selector = Selector::parse(".list-title").expect("constant selector");
    let title_selector = Selector::parse(".title").expect("constant selector");
    let author_selector = Selector::parse(".authors").expect("constant selector");
    let abstract_selector = Selector::parse(".abstract-full").expect("constant selector");
    let mut papers = Vec::new();
    for listing in tree.select(&listing_selector) {
        let text_of = |selector: &Selector| {
            listing
                .select(selector)
                .next()
                .map(|element| compact(&element.text().collect::<Vec<_>>().join(" ")))
                .unwrap_or_default()
        };
        let identification = text_of(&id_selector);
        let identifier = identification
            .split_once("arXiv:")
            .map(|(_, tail)| {
                tail.trim_start()
                    .chars()
                    .take_while(|character| {
                        character.is_alphanumeric() || matches!(character, '_' | '.' | '-')
                    })
                    .collect::<String>()
            })
            .unwrap_or_default();
        if identifier.is_empty() {
            continue;
        }
        let authors = text_of(&author_selector)
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty() && !name.to_lowercase().starts_with("authors:"))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let listing_text = compact(&listing.text().collect::<Vec<_>>().join(" "));
        let words = listing_text.split_whitespace().collect::<Vec<_>>();
        let year = words
            .windows(3)
            .find(|words| {
                words[0] == "announced"
                    && words[2].len() == 4
                    && (words[2].starts_with("19") || words[2].starts_with("20"))
                    && words[2].chars().all(|character| character.is_ascii_digit())
            })
            .map(|words| words[2])
            .unwrap_or("");
        papers.push(json!({"id": format!("arxiv:{identifier}"), "url": format!("https://arxiv.org/abs/{identifier}"),
            "title": text_of(&title_selector), "authors": authors, "year": year,
            "abstract": text_of(&abstract_selector), "publication_type": "preprint", "evidence_level": "listing",
            "fulltext_urls": [format!("https://arxiv.org/pdf/{identifier}")]}));
        if papers.len() >= request.limit as usize {
            break;
        }
    }
    Ok(
        json!({"source": "arxiv", "query": request.query, "request_url": document.final_url, "papers": papers,
        "note": "the API endpoint was unavailable; these came from the search page, so they carry less metadata and are ordered less sharply than a relevance-ranked API response would be"}),
    )
}
