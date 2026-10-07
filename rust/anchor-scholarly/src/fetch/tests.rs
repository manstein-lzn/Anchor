use std::{
    collections::VecDeque,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use reqwest::{
    Url,
    header::{HeaderMap, HeaderValue, LOCATION, RETRY_AFTER},
};
use tokio::{
    sync::Mutex,
    time::{Instant, sleep},
};

use crate::{
    Error, MAX_RESPONSE_BYTES,
    transport::{HttpResponse, PublicHttpsTransport, Transport},
};

use super::{Fetcher, retry_after};

#[derive(Default)]
struct Scripted {
    answers: Mutex<VecDeque<HttpResponse>>,
    resolutions: Mutex<VecDeque<Vec<IpAddr>>>,
    requests: Mutex<Vec<(String, SocketAddr, Instant)>>,
    resolve_calls: Mutex<Vec<String>>,
    delay: Duration,
    dns_delay: Duration,
}

impl Scripted {
    fn response(status: u16) -> HttpResponse {
        HttpResponse {
            status,
            headers: HeaderMap::new(),
            body: b"ok".to_vec(),
        }
    }

    fn redirect(location: &str) -> HttpResponse {
        let mut response = Self::response(302);
        response
            .headers
            .insert(LOCATION, HeaderValue::from_str(location).unwrap());
        response
    }

    fn retry(status: u16, delay: &str) -> HttpResponse {
        let mut response = Self::response(status);
        response
            .headers
            .insert(RETRY_AFTER, HeaderValue::from_str(delay).unwrap());
        response
    }

    fn new(responses: Vec<HttpResponse>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(responses.into()),
            ..Self::default()
        })
    }
}

#[async_trait]
impl Transport for Scripted {
    async fn resolve(&self, hostname: &str) -> Result<Vec<IpAddr>, Error> {
        self.resolve_calls.lock().await.push(hostname.to_owned());
        sleep(self.dns_delay).await;
        Ok(self
            .resolutions
            .lock()
            .await
            .pop_front()
            .unwrap_or_else(|| vec!["93.184.216.34".parse().unwrap()]))
    }

    async fn get(
        &self,
        url: &Url,
        pinned_address: SocketAddr,
        _timeout: Duration,
    ) -> Result<HttpResponse, Error> {
        self.requests
            .lock()
            .await
            .push((url.to_string(), pinned_address, Instant::now()));
        sleep(self.delay).await;
        self.answers
            .lock()
            .await
            .pop_front()
            .ok_or_else(|| Error::malformed("fixture exhausted"))
    }
}

#[tokio::test(start_paused = true)]
async fn retries_pin_one_dns_answer_and_cooldown_rejects_new_requests() {
    let transport = Scripted::new(vec![
        Scripted::retry(429, "0"),
        Scripted::retry(429, "0"),
        Scripted::response(200),
    ]);
    let fetcher = Fetcher::new(transport.clone());
    let error = fetcher
        .fetch("https://api.crossref.org/works", Duration::from_secs(60))
        .await
        .unwrap_err();
    assert_eq!(error.code, "source_rate_limited");
    assert!(error.retryable);
    let calls = transport.requests.lock().await;
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].1, calls[1].1);
    assert_eq!(calls[1].2 - calls[0].2, Duration::from_secs(1));
    drop(calls);
    assert_eq!(transport.resolve_calls.lock().await.len(), 1);
    let error = fetcher
        .fetch("https://api.crossref.org/works", Duration::from_secs(60))
        .await
        .unwrap_err();
    assert_eq!(error.code, "source_rate_limited");
    assert_eq!(transport.requests.lock().await.len(), 2);
    sleep(Duration::from_secs(30)).await;
    assert!(
        fetcher
            .fetch("https://api.crossref.org/works", Duration::from_secs(60))
            .await
            .is_ok()
    );
    assert_eq!(transport.requests.lock().await.len(), 3);
}

#[tokio::test(start_paused = true)]
async fn redirects_resolve_again_and_do_not_reuse_an_unvalidated_connection() {
    let transport = Scripted::new(vec![Scripted::redirect("/next"), Scripted::response(200)]);
    transport.resolutions.lock().await.extend([
        vec!["93.184.216.34".parse().unwrap()],
        vec!["127.0.0.1".parse().unwrap()],
    ]);
    let error = Fetcher::new(transport.clone())
        .fetch("https://papers.example.org/first", Duration::from_secs(60))
        .await
        .unwrap_err();
    assert_eq!(error.code, "invalid_request");
    assert_eq!(transport.requests.lock().await.len(), 1);
    assert_eq!(transport.resolve_calls.lock().await.len(), 2);
}

#[tokio::test]
async fn mixed_dns_answers_never_reach_the_transport() {
    let transport = Scripted::new(vec![Scripted::response(200)]);
    transport.resolutions.lock().await.push_back(vec![
        "93.184.216.34".parse().unwrap(),
        "10.0.0.1".parse().unwrap(),
    ]);
    assert!(
        Fetcher::new(transport.clone())
            .fetch("https://papers.example.org", Duration::from_secs(60))
            .await
            .is_err()
    );
    assert!(transport.requests.lock().await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn arxiv_api_and_web_share_pacing_and_openalex_and_crossref_are_independent() {
    let transport = Scripted::new((0..5).map(|_| Scripted::response(200)).collect());
    let fetcher = Fetcher::new(transport.clone());
    for url in [
        "https://export.arxiv.org/api/query",
        "https://arxiv.org/search/",
        "https://api.crossref.org/works",
        "https://api.openalex.org/works",
        "https://api.openalex.org./works",
    ] {
        fetcher.fetch(url, Duration::from_secs(60)).await.unwrap();
    }
    let calls = transport.requests.lock().await;
    assert_eq!(calls[1].2 - calls[0].2, Duration::from_secs(3));
    assert_eq!(calls[3].2, calls[2].2);
    assert_eq!(calls[4].2 - calls[3].2, Duration::from_secs(1));
}

#[tokio::test(start_paused = true)]
async fn timeouts_cover_dns_locks_pacing_and_retry_delays() {
    let transport = Scripted::new(vec![Scripted::retry(503, "30")]);
    let started = Instant::now();
    let error = Fetcher::new(transport.clone())
        .fetch("https://api.crossref.org/works", Duration::from_secs(2))
        .await
        .unwrap_err();
    assert_eq!(error.code, "source_timeout");
    assert_eq!(Instant::now() - started, Duration::from_secs(2));
    assert_eq!(transport.requests.lock().await.len(), 1);
    let transport = Arc::new(Scripted {
        dns_delay: Duration::from_secs(10),
        ..Scripted::default()
    });
    let started = Instant::now();
    let error = Fetcher::new(transport.clone())
        .fetch("https://papers.example.org", Duration::from_secs(2))
        .await
        .unwrap_err();
    assert_eq!(error.code, "source_timeout");
    assert_eq!(Instant::now() - started, Duration::from_secs(2));
    assert!(transport.requests.lock().await.is_empty());
    let transport = Scripted::new(vec![Scripted::response(200), Scripted::response(200)]);
    let fetcher = Fetcher::new(transport.clone());
    fetcher
        .fetch("https://api.crossref.org", Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(
        fetcher
            .fetch("https://api.crossref.org", Duration::from_millis(100))
            .await
            .unwrap_err()
            .code,
        "source_timeout"
    );
    assert_eq!(transport.requests.lock().await.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn concurrent_requests_are_serialized_at_the_same_host() {
    let transport = Arc::new(Scripted {
        answers: Mutex::new(vec![Scripted::response(200), Scripted::response(200)].into()),
        delay: Duration::from_secs(2),
        ..Scripted::default()
    });
    let fetcher = Fetcher::new(transport.clone());
    let (first, second) = tokio::join!(
        fetcher.fetch("https://api.crossref.org/works", Duration::from_secs(60)),
        fetcher.fetch("https://api.crossref.org/works", Duration::from_secs(60))
    );
    assert!(first.is_ok() && second.is_ok());
    let calls = transport.requests.lock().await;
    assert_eq!(calls[1].2 - calls[0].2, Duration::from_secs(2));
}

#[tokio::test(start_paused = true)]
async fn host_lock_wait_is_included_in_the_second_requests_timeout() {
    let transport = Arc::new(Scripted {
        answers: Mutex::new(vec![Scripted::response(200)].into()),
        delay: Duration::from_secs(2),
        ..Scripted::default()
    });
    let fetcher = Fetcher::new(transport.clone());
    let (first, second) = tokio::join!(
        fetcher.fetch("https://api.crossref.org/works", Duration::from_secs(5)),
        fetcher.fetch("https://api.crossref.org/works", Duration::from_millis(100))
    );
    assert!(first.is_ok());
    assert_eq!(second.unwrap_err().code, "source_timeout");
    assert_eq!(transport.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn redirects_cannot_downgrade_or_enter_private_urls() {
    for location in [
        "http://example.org",
        "https://localhost:444",
        "https://127.0.0.1",
        "https://user:pass@example.org",
        "https://example.org\\@127.0.0.1",
    ] {
        let transport = Scripted::new(vec![Scripted::redirect(location)]);
        assert!(
            Fetcher::new(transport.clone())
                .fetch("https://papers.example.org", Duration::from_secs(60))
                .await
                .is_err(),
            "{location}"
        );
        assert_eq!(transport.requests.lock().await.len(), 1);
    }
}

#[tokio::test]
async fn redirect_count_and_missing_location_fail_closed() {
    let transport = Scripted::new((0..6).map(|_| Scripted::redirect("/next")).collect());
    let error = Fetcher::new(transport.clone())
        .fetch("https://papers.example.org", Duration::from_secs(60))
        .await
        .unwrap_err();
    assert_eq!(error.code, "invalid_request");
    assert_eq!(transport.requests.lock().await.len(), 6);
    let transport = Scripted::new(vec![Scripted::response(302)]);
    assert_eq!(
        Fetcher::new(transport)
            .fetch("https://papers.example.org", Duration::from_secs(60))
            .await
            .unwrap_err()
            .code,
        "invalid_redirect"
    );
}

#[tokio::test(start_paused = true)]
async fn transient_server_statuses_retry_once_and_access_errors_do_not_retry() {
    for status in [500, 502, 503, 504] {
        let transport = Scripted::new(vec![
            Scripted::retry(status, "0"),
            Scripted::retry(status, "0"),
        ]);
        let error = Fetcher::new(transport.clone())
            .fetch("https://papers.example.org", Duration::from_secs(60))
            .await
            .unwrap_err();
        assert_eq!(error.code, "source_unavailable");
        assert!(error.retryable);
        assert_eq!(transport.requests.lock().await.len(), 2);
    }
    for (status, code) in [
        (401, "source_access_denied"),
        (403, "source_access_denied"),
        (404, "source_not_found"),
        (418, "source_http_error"),
    ] {
        let transport = Scripted::new(vec![Scripted::response(status)]);
        let error = Fetcher::new(transport.clone())
            .fetch("https://papers.example.org", Duration::from_secs(60))
            .await
            .unwrap_err();
        assert_eq!(error.code, code);
        assert!(!error.retryable);
        assert_eq!(transport.requests.lock().await.len(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn a_successful_retry_does_not_install_a_cooldown() {
    let transport = Scripted::new(vec![
        Scripted::retry(503, "0"),
        Scripted::response(200),
        Scripted::response(200),
    ]);
    let fetcher = Fetcher::new(transport.clone());
    assert!(
        fetcher
            .fetch("https://papers.example.org", Duration::from_secs(5))
            .await
            .is_ok()
    );
    assert!(
        fetcher
            .fetch("https://papers.example.org", Duration::from_secs(5))
            .await
            .is_ok()
    );
    assert_eq!(transport.requests.lock().await.len(), 3);
}

#[tokio::test]
async fn injected_transports_cannot_bypass_the_response_size_limit() {
    let mut response = Scripted::response(200);
    response.body = vec![b' '; MAX_RESPONSE_BYTES + 1];
    let error = Fetcher::new(Scripted::new(vec![response]))
        .fetch("https://papers.example.org", Duration::from_secs(60))
        .await
        .unwrap_err();
    assert_eq!(error.code, "source_too_large");
}

#[tokio::test]
async fn production_transport_has_no_private_address_escape_hatch() {
    let transport = PublicHttpsTransport;
    let error = transport
        .get(
            &Url::parse("https://api.crossref.org/works").unwrap(),
            "127.0.0.1:443".parse().unwrap(),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "invalid_request");
}

#[test]
fn retry_after_seconds_dates_invalid_and_nonfinite_values_are_bounded() {
    assert_eq!(retry_after(Some("999999"), 3.0), Duration::from_secs(30));
    assert_eq!(retry_after(Some("-1"), 3.0), Duration::ZERO);
    assert_eq!(retry_after(Some("0.5"), 3.0), Duration::from_millis(500));
    for value in [None, Some("invalid"), Some("NaN"), Some("inf")] {
        assert_eq!(retry_after(value, 3.0), Duration::from_secs(3));
    }
    let future = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(120));
    assert_eq!(retry_after(Some(&future), 3.0), Duration::from_secs(30));
    assert_eq!(
        retry_after(Some("Sun, 06 Nov 1994 08:49:37 GMT"), 3.0),
        Duration::ZERO
    );
}
