use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use async_trait::async_trait;
use reqwest::{Url, header::HeaderMap};

use crate::{
    Error, MAX_RESPONSE_BYTES,
    address::{select_address, validate_url},
};

#[derive(Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

#[async_trait]
pub trait Transport: Send + Sync {
    async fn resolve(&self, hostname: &str) -> Result<Vec<IpAddr>, Error>;
    async fn get(
        &self,
        url: &Url,
        pinned_address: SocketAddr,
        timeout: Duration,
    ) -> Result<HttpResponse, Error>;
}

#[derive(Default)]
pub struct PublicHttpsTransport;

#[async_trait]
impl Transport for PublicHttpsTransport {
    async fn resolve(&self, hostname: &str) -> Result<Vec<IpAddr>, Error> {
        if let Ok(address) = hostname.trim_matches(['[', ']']).parse::<IpAddr>() {
            return Ok(vec![address]);
        }
        tokio::net::lookup_host((hostname, 443))
            .await
            .map(|addresses| addresses.map(|address| address.ip()).collect())
            .map_err(|_| Error::source("source_unavailable", "source DNS lookup failed", true))
    }

    async fn get(
        &self,
        url: &Url,
        pinned_address: SocketAddr,
        timeout: Duration,
    ) -> Result<HttpResponse, Error> {
        validate_url(url.as_str())?;
        select_address(vec![pinned_address.ip()])?;
        if pinned_address.port() != 443 {
            return Err(Error::input("research transport requires port 443"));
        }
        let hostname = url
            .host_str()
            .ok_or_else(|| Error::input("missing hostname"))?;
        if hostname
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .ok()
            .is_some_and(|address| address != pinned_address.ip())
        {
            return Err(Error::input(
                "pinned address does not match the URL address",
            ));
        }
        let client = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .resolve(hostname, pinned_address)
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(10)))
            .user_agent("AnchorAcademicResearch/0.1 (read-only literature research)")
            .build()
            .map_err(request_error)?;
        let response = client
            .get(url.clone())
            .header(
                "Accept",
                "application/json, application/atom+xml, text/html, application/pdf, text/plain",
            )
            .header("Accept-Encoding", "identity")
            .send()
            .await
            .map_err(request_error)?;
        bounded_response(response).await
    }
}

pub async fn bounded_response(mut response: reqwest::Response) -> Result<HttpResponse, Error> {
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let mut body = Vec::new();
    if status == 200 {
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
        {
            return Err(Error::too_large());
        }
        while let Some(chunk) = response.chunk().await.map_err(request_error)? {
            if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
                return Err(Error::too_large());
            }
            body.extend_from_slice(&chunk);
        }
    }
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

fn request_error(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::timeout()
    } else {
        Error::source("source_unavailable", "source request failed", true)
    }
}
