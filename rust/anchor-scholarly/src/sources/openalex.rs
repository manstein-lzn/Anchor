use std::{collections::BTreeMap, time::Duration};

use clap::ValueEnum;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{Error, MAX_RESPONSE_BYTES, SearchRequest, fetch::Fetcher, validate_options};

use super::{endpoint, normalize_url};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum Direction {
    #[default]
    #[value(name = "cited_by")]
    CitedBy,
    Cites,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CitedBy => "cited_by",
            Self::Cites => "cites",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CitationRequest {
    pub identifier: String,
    pub direction: Direction,
    pub limit: i64,
}

impl CitationRequest {
    pub fn validate(&self) -> Result<(), Error> {
        if self.identifier.is_empty() || self.identifier.chars().count() > 300 {
            return Err(Error::input("identifier must contain 1 to 300 characters"));
        }
        if self.identifier.chars().any(char::is_control) || self.identifier.contains('\\') {
            return Err(Error::input("identifier contains forbidden characters"));
        }
        validate_options(self.limit, 0)?;
        work_url(&self.identifier)?;
        Ok(())
    }
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    meta: Meta,
    results: Vec<Work>,
}

#[derive(Default, Deserialize)]
struct Meta {
    count: Option<Value>,
}

#[derive(Deserialize)]
struct Work {
    id: String,
    doi: Option<String>,
    title: Option<String>,
    #[serde(default)]
    authorships: Vec<Authorship>,
    publication_year: Option<Value>,
    primary_location: Option<Location>,
    best_oa_location: Option<Location>,
    #[serde(default)]
    locations: Vec<Location>,
    #[serde(rename = "type")]
    publication_type: Option<String>,
    abstract_inverted_index: Option<Value>,
    cited_by_count: Option<Value>,
    referenced_works: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct Authorship {
    author: Option<Author>,
}

#[derive(Deserialize)]
struct Author {
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct Location {
    pdf_url: Option<String>,
    landing_page_url: Option<String>,
    source: Option<Publication>,
}

#[derive(Deserialize)]
struct Publication {
    display_name: Option<String>,
}

pub(super) async fn search(
    fetcher: &Fetcher,
    request: &SearchRequest,
    timeout: Duration,
) -> Result<Value, Error> {
    let page = request.offset as u64 / request.limit as u64 + 1;
    let url = endpoint(
        "https://api.openalex.org/works",
        &[
            (
                "filter",
                format!("title_and_abstract.search:{}", request.query),
            ),
            ("per-page", request.limit.to_string()),
            ("page", page.to_string()),
        ],
    )?;
    let document = fetcher.fetch(url.as_str(), timeout).await?;
    let envelope: Envelope = serde_json::from_slice(&document.body)
        .map_err(|_| Error::malformed("invalid OpenAlex JSON response"))?;
    let mut remaining_abstract_bytes = MAX_RESPONSE_BYTES;
    let papers = envelope
        .results
        .into_iter()
        .map(|work| paper(work, &mut remaining_abstract_bytes))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(
        json!({"source": "openalex", "query": request.query, "request_url": document.final_url,
        "total_results": envelope.meta.count, "papers": papers}),
    )
}

pub(crate) async fn citations(
    fetcher: &Fetcher,
    request: &CitationRequest,
    timeout: Duration,
) -> Result<Value, Error> {
    request.validate()?;
    let url = work_url(&request.identifier)?;
    let document = fetcher.fetch(url.as_str(), timeout).await?;
    let seed: Work = serde_json::from_slice(&document.body)
        .map_err(|_| Error::malformed("invalid OpenAlex work response"))?;
    let seed_id = work_id(&seed.id)?;
    let mut remaining_abstract_bytes = MAX_RESPONSE_BYTES;
    let (papers, total) = match request.direction {
        Direction::CitedBy => {
            let url = endpoint(
                "https://api.openalex.org/works",
                &[
                    ("filter", format!("cites:{seed_id}")),
                    ("per-page", request.limit.to_string()),
                    ("sort", "cited_by_count:desc".to_owned()),
                ],
            )?;
            let envelope = fetch_works(fetcher, &url, timeout).await?;
            let papers = envelope
                .results
                .into_iter()
                .map(|work| paper(work, &mut remaining_abstract_bytes))
                .collect::<Result<Vec<_>, _>>()?;
            (papers, envelope.meta.count)
        }
        Direction::Cites => {
            let referenced = seed.referenced_works.unwrap_or_default();
            let total = referenced.len();
            let referenced = referenced
                .iter()
                .take(200)
                .map(|identifier| work_id(identifier))
                .collect::<Result<Vec<_>, _>>()?;
            let mut papers = Vec::new();
            for chunk in referenced.chunks(50) {
                let url = endpoint(
                    "https://api.openalex.org/works",
                    &[
                        ("filter", format!("openalex_id:{}", chunk.join("|"))),
                        ("per-page", "50".to_owned()),
                    ],
                )?;
                let envelope = fetch_works(fetcher, &url, timeout).await?;
                for work in envelope.results {
                    papers.push(paper(work, &mut remaining_abstract_bytes)?);
                }
            }
            papers.truncate(request.limit as usize);
            (papers, Some(json!(total)))
        }
    };
    Ok(json!({
        "source": "openalex", "identifier": request.identifier,
        "direction": request.direction.as_str(), "total_results": total, "papers": papers,
    }))
}

async fn fetch_works(
    fetcher: &Fetcher,
    url: &reqwest::Url,
    timeout: Duration,
) -> Result<Envelope, Error> {
    let document = fetcher.fetch(url.as_str(), timeout).await?;
    serde_json::from_slice(&document.body)
        .map_err(|_| Error::malformed("invalid OpenAlex JSON response"))
}

fn work_url(identifier: &str) -> Result<reqwest::Url, Error> {
    let unsupported = || {
        Error::source(
            "unsupported_identifier",
            "identifier must be doi:<doi>, arxiv:<id> or openalex:<work id>",
            false,
        )
    };
    let (kind, value) = identifier.split_once(':').ok_or_else(unsupported)?;
    if value.is_empty() {
        return Err(unsupported());
    }
    let key = match kind {
        "openalex" => {
            if !is_work_id(value) {
                return Err(unsupported());
            }
            value.to_owned()
        }
        "doi" => format!("doi:{value}"),
        "arxiv" => format!("doi:10.48550/arXiv.{value}"),
        _ => return Err(unsupported()),
    };
    let mut url = endpoint("https://api.openalex.org/works", &[])?;
    url.set_path(&format!("/works/{key}"));
    Ok(url)
}

fn is_work_id(value: &str) -> bool {
    value.strip_prefix('W').is_some_and(|number| {
        !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn work_id(value: &str) -> Result<&str, Error> {
    let identifier = value.rsplit('/').next().unwrap_or_default();
    if !is_work_id(identifier) {
        return Err(Error::malformed("invalid OpenAlex work identifier"));
    }
    Ok(identifier)
}

fn abstract_text(index: Option<Value>, remaining: &mut usize) -> Result<String, Error> {
    let Some(Value::Object(index)) = index else {
        return Ok(String::new());
    };
    let mut positions = BTreeMap::new();
    for (word, offsets) in &index {
        if offsets.is_null() {
            continue;
        }
        let offsets = offsets
            .as_array()
            .ok_or_else(|| Error::malformed("invalid OpenAlex abstract offsets"))?;
        for offset in offsets {
            let offset = offset
                .as_i64()
                .ok_or_else(|| Error::malformed("invalid OpenAlex abstract position"))?;
            positions.insert(offset, word.as_str());
        }
    }
    let mut text = String::new();
    for word in positions.into_values() {
        let length = word.len() + usize::from(!text.is_empty());
        if length > *remaining {
            return Err(Error::too_large());
        }
        *remaining -= length;
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(word);
    }
    Ok(text)
}

fn paper(work: Work, remaining: &mut usize) -> Result<Value, Error> {
    let doi = work
        .doi
        .unwrap_or_default()
        .trim_start_matches("https://doi.org/")
        .to_owned();
    let abstract_text = abstract_text(work.abstract_inverted_index, remaining)?;
    let mut urls = Vec::new();
    for location in work.best_oa_location.into_iter().chain(work.locations) {
        for url in [location.pdf_url, location.landing_page_url]
            .into_iter()
            .flatten()
        {
            if !url.is_empty() && !urls.contains(&url) {
                urls.push(url);
            }
        }
    }
    let fulltext_urls = urls
        .iter()
        .map(|url| normalize_url(url))
        .collect::<Result<Vec<_>, _>>()?;
    let authors = work
        .authorships
        .into_iter()
        .filter_map(|entry| entry.author)
        .filter_map(|author| author.display_name)
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    let venue = work
        .primary_location
        .and_then(|location| location.source)
        .and_then(|publication| publication.display_name)
        .unwrap_or_default();
    let identifier = if doi.is_empty() {
        format!("openalex:{}", work.id.rsplit('/').next().unwrap_or(""))
    } else {
        format!("doi:{doi}")
    };
    let url = if doi.is_empty() {
        work.id
    } else {
        format!("https://doi.org/{doi}")
    };
    Ok(json!({
        "id": identifier, "doi": if doi.is_empty() { None } else { Some(doi) },
        "title": work.title.unwrap_or_default(), "authors": authors, "year": work.publication_year,
        "venue": venue, "publication_type": work.publication_type, "url": url,
        "evidence_level": if abstract_text.is_empty() { "metadata_only" } else { "abstract" },
        "abstract": abstract_text, "fulltext_urls": fulltext_urls, "cited_by_count": work.cited_by_count,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::abstract_text;

    #[test]
    fn inverted_index_expansion_is_bounded() {
        let error = abstract_text(Some(json!({"large": [0, 1, 2]})), &mut 8).unwrap_err();
        assert_eq!(error.code, "source_too_large");
    }
}
