mod arxiv;
mod crossref;
pub(crate) mod openalex;

use reqwest::Url;
use scraper::Html;

use crate::{Error, SearchRequest, Source, fetch::Fetcher};

pub(crate) async fn search(
    fetcher: &Fetcher,
    request: &SearchRequest,
    timeout: std::time::Duration,
) -> Result<serde_json::Value, Error> {
    request.validate()?;
    match request.source {
        Source::Crossref => crossref::search(fetcher, request, timeout).await,
        Source::Openalex => openalex::search(fetcher, request, timeout).await,
        Source::Arxiv => arxiv::search(fetcher, request, timeout).await,
    }
}

fn endpoint(base: &str, parameters: &[(&str, String)]) -> Result<Url, Error> {
    let mut url = Url::parse(base).map_err(|_| Error::input("invalid source endpoint"))?;
    url.query_pairs_mut().extend_pairs(
        parameters
            .iter()
            .map(|(name, value)| (*name, value.as_str())),
    );
    Ok(url)
}

fn normalize_url(value: &str) -> Result<String, Error> {
    let mut url = Url::parse(value).map_err(|_| Error::malformed("invalid source metadata URL"))?;
    if matches!(url.scheme(), "http" | "https") {
        url.set_scheme("https")
            .map_err(|_| Error::malformed("invalid metadata URL scheme"))?;
    }
    if url.host_str() == Some("xplorestaging.ieee.org") {
        url.set_host(Some("ieeexplore.ieee.org"))
            .map_err(|_| Error::malformed("invalid metadata hostname"))?;
    }
    if url.has_host() {
        let _ = url.set_username("");
        let _ = url.set_password(None);
    }
    Ok(url.to_string())
}

fn plain_markup(value: &str) -> String {
    if value.trim().is_empty() {
        return String::new();
    }
    Html::parse_fragment(value)
        .root_element()
        .text()
        .collect::<String>()
        .trim()
        .to_owned()
}

fn compact(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}
