use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{Error, SearchRequest, fetch::Fetcher};

use super::{endpoint, normalize_url, plain_markup};

#[derive(Deserialize)]
struct Envelope {
    message: Message,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    items: Vec<Work>,
    #[serde(rename = "total-results")]
    total_results: Option<Value>,
}

#[derive(Deserialize)]
struct Work {
    #[serde(rename = "DOI")]
    doi: String,
    #[serde(default)]
    title: Vec<String>,
    #[serde(default)]
    author: Vec<Author>,
    published: Option<Date>,
    #[serde(default, rename = "container-title")]
    venue: Vec<String>,
    #[serde(rename = "type")]
    publication_type: Option<String>,
    #[serde(rename = "URL")]
    url: Option<String>,
    #[serde(rename = "abstract")]
    abstract_text: Option<String>,
    #[serde(default)]
    link: Vec<Link>,
}

#[derive(Deserialize)]
struct Author {
    given: Option<String>,
    family: Option<String>,
}

#[derive(Deserialize)]
struct Date {
    #[serde(default, rename = "date-parts")]
    parts: Vec<Vec<Value>>,
}

#[derive(Deserialize)]
struct Link {
    #[serde(rename = "URL")]
    url: Option<String>,
}

pub(super) async fn search(
    fetcher: &Fetcher,
    request: &SearchRequest,
    timeout: Duration,
) -> Result<Value, Error> {
    let url = endpoint(
        "https://api.crossref.org/works",
        &[
            ("query.bibliographic", request.query.clone()),
            ("rows", request.limit.to_string()),
        ],
    )?;
    let document = fetcher.fetch(url.as_str(), timeout).await?;
    let envelope: Envelope = serde_json::from_slice(&document.body)
        .map_err(|_| Error::malformed("invalid Crossref JSON response"))?;
    let papers = envelope
        .message
        .items
        .into_iter()
        .map(paper)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(
        json!({"source": "crossref", "query": request.query, "request_url": document.final_url,
        "total_results": envelope.message.total_results, "papers": papers}),
    )
}

fn paper(work: Work) -> Result<Value, Error> {
    let fallback = format!("https://doi.org/{}", work.doi);
    let url = normalize_url(work.url.as_deref().unwrap_or(&fallback))?;
    let fulltext_urls = work
        .link
        .into_iter()
        .filter_map(|link| link.url)
        .map(|url| normalize_url(&url))
        .collect::<Result<Vec<_>, _>>()?;
    let year = work
        .published
        .and_then(|date| date.parts.into_iter().next())
        .and_then(|parts| parts.into_iter().next());
    let authors = work
        .author
        .into_iter()
        .map(|author| {
            [author.given, author.family]
                .into_iter()
                .flatten()
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>();
    let raw_abstract = work.abstract_text.unwrap_or_default();
    Ok(json!({
        "id": format!("doi:{}", work.doi), "doi": work.doi,
        "title": work.title.join(" "), "authors": authors, "year": year,
        "venue": work.venue.join(" "), "publication_type": work.publication_type,
        "url": url, "abstract": plain_markup(&raw_abstract), "fulltext_urls": fulltext_urls,
        "evidence_level": if raw_abstract.is_empty() { "metadata_only" } else { "abstract" },
    }))
}
