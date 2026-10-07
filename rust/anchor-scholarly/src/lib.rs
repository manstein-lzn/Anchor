mod address;
pub mod cli;
mod documents;
mod error;
mod fetch;
pub mod mcp;
mod queries;
mod sources;
pub mod transport;

use std::{sync::Arc, time::Duration};

use chrono::{SecondsFormat, Utc};
use clap::ValueEnum;
use serde_json::{Value, json};
use tokio::time::Instant;

pub use documents::{ReadManyRequest, ReadRequest};
pub use error::Error;
pub use queries::read_queries;
pub use sources::openalex::{CitationRequest, Direction};
use transport::{PublicHttpsTransport, Transport};

pub const MAX_RESPONSE_BYTES: usize = 8_000_000;
pub const BATCH_READ_LIMIT: usize = 8;
pub const READ_CHARACTER_LIMIT: usize = 24_000;
pub const PDF_PAGE_LIMIT: usize = 40;
pub const BATCH_SEARCH_LIMIT: usize = 40;
pub const BATCH_BUDGET_SECONDS: u64 = 420;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum Source {
    #[default]
    Crossref,
    Arxiv,
    Openalex,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Crossref => "crossref",
            Self::Arxiv => "arxiv",
            Self::Openalex => "openalex",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SearchRequest {
    pub query: String,
    pub source: Source,
    pub limit: i64,
    pub offset: i64,
}

impl SearchRequest {
    pub fn validate(&self) -> Result<(), Error> {
        if self.query.is_empty() || self.query.chars().count() > 1000 {
            return Err(Error::input("query must contain 1 to 1000 characters"));
        }
        validate_options(self.limit, self.offset)
    }
}

fn validate_options(limit: i64, offset: i64) -> Result<(), Error> {
    if !(1..=100).contains(&limit) {
        return Err(Error::input("limit must be between 1 and 100"));
    }
    if offset < 0 {
        return Err(Error::input("offset must be non-negative"));
    }
    Ok(())
}

pub struct Scholarly {
    fetcher: fetch::Fetcher,
}

impl Default for Scholarly {
    fn default() -> Self {
        Self::new()
    }
}

impl Scholarly {
    pub fn new() -> Self {
        Self::with_transport(Arc::new(PublicHttpsTransport))
    }

    pub fn with_transport(transport: Arc<dyn Transport>) -> Self {
        Self {
            fetcher: fetch::Fetcher::new(transport),
        }
    }

    pub async fn search(&self, request: &SearchRequest, timeout: Duration) -> Result<Value, Error> {
        let result = sources::search(&self.fetcher, request, timeout).await?;
        Ok(stamp(result, true))
    }

    pub async fn read(&self, request: &ReadRequest, timeout: Duration) -> Result<Value, Error> {
        Ok(stamp(
            documents::read(&self.fetcher, request, timeout).await?,
            true,
        ))
    }

    pub async fn read_many(
        &self,
        request: &ReadManyRequest,
        timeout: Duration,
    ) -> Result<Value, Error> {
        Ok(stamp(
            documents::read_many(&self.fetcher, request, timeout).await?,
            true,
        ))
    }

    pub async fn citations(
        &self,
        request: &CitationRequest,
        timeout: Duration,
    ) -> Result<Value, Error> {
        Ok(stamp(
            sources::openalex::citations(&self.fetcher, request, timeout).await?,
            true,
        ))
    }

    pub async fn sources(&self, timeout: Duration) -> Value {
        let mut sources = Vec::new();
        let mut usable = Vec::new();
        for source in [Source::Crossref, Source::Openalex, Source::Arxiv] {
            let request = SearchRequest {
                query: "cost model compiler".to_owned(),
                source,
                limit: 1,
                offset: 0,
            };
            let (answered, detail) = match sources::search(
                &self.fetcher,
                &request,
                timeout.min(Duration::from_secs(30)),
            )
            .await
            {
                Ok(result) => {
                    usable.push(source.as_str());
                    let count = result["papers"].as_array().map_or(0, Vec::len);
                    (true, format!("{count} result(s) for a probe query"))
                }
                Err(error) => (false, error.summary(120)),
            };
            sources.push(json!({"source": source.as_str(), "usable": answered, "detail": detail}));
        }
        let note = if usable.is_empty() {
            "none answered; report that rather than working around it"
        } else {
            "use the ones that answered; a source that refused is not a dead end, it is a source to leave alone for a while"
        };
        stamp(
            json!({"sources": sources, "usable": usable, "note": note}),
            false,
        )
    }

    pub async fn search_many(
        &self,
        queries: Vec<String>,
        source: Source,
        limit: i64,
        offset: i64,
        budget_seconds: i64,
        timeout: Duration,
    ) -> Result<Value, Error> {
        validate_options(limit, offset)?;
        if queries.len() > BATCH_SEARCH_LIMIT {
            return Err(Error::input("search-many accepts at most 40 queries"));
        }
        if !(0..=3600).contains(&budget_seconds) {
            return Err(Error::input("budget must be between 0 and 3600 seconds"));
        }
        let queries = queries
            .into_iter()
            .map(|query| query.trim().to_owned())
            .filter(|query| !query.is_empty())
            .collect::<Vec<_>>();
        if queries.is_empty() {
            return Err(Error::input("scholarly.search_many requires queries"));
        }
        let budget = Duration::from_secs(if budget_seconds == 0 {
            BATCH_BUDGET_SECONDS
        } else {
            budget_seconds as u64
        });
        let started = Instant::now();
        let mut results = Vec::new();
        for query in &queries {
            let remaining = budget.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                break;
            }
            let request = SearchRequest {
                query: query.clone(),
                source,
                limit,
                offset,
            };
            let result = match sources::search(&self.fetcher, &request, timeout.min(remaining))
                .await
            {
                Ok(result) => result,
                Err(error) => json!({"query": query, "error": error.summary(200), "papers": []}),
            };
            results.push(result);
        }
        let attempted = results.len();
        let with_results = results
            .iter()
            .filter(|result| {
                result["papers"]
                    .as_array()
                    .is_some_and(|papers| !papers.is_empty())
            })
            .count();
        Ok(stamp(
            json!({"source": source.as_str(), "queries": queries, "results": results,
            "with_results": with_results, "attempted": attempted, "not_attempted": &queries[attempted..],
            "ran_out_of_time": attempted < queries.len()}),
            true,
        ))
    }
}

fn stamp(mut result: Value, untrusted: bool) -> Value {
    result["retrieved_at"] =
        Value::String(Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false));
    if untrusted {
        result["untrusted_source_content"] = Value::Bool(true);
    }
    result
}
