use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, SystemTime},
};

use reqwest::header::{CONTENT_TYPE, LOCATION, RETRY_AFTER};
use tokio::{
    sync::Mutex,
    time::{Instant, sleep, sleep_until, timeout_at},
};

use crate::{
    Error, MAX_RESPONSE_BYTES,
    address::{select_address, validate_url},
    transport::Transport,
};

#[derive(Default)]
struct HostState {
    blocked_until: Option<Instant>,
}

pub(crate) struct Fetcher {
    transport: Arc<dyn Transport>,
    hosts: Mutex<HashMap<String, Arc<Mutex<HostState>>>>,
    pacing: Mutex<HashMap<&'static str, Arc<Mutex<Option<Instant>>>>>,
}

#[derive(Debug)]
pub(crate) struct Document {
    pub final_url: String,
    pub content_type: String,
    pub body: Vec<u8>,
}

impl Fetcher {
    pub fn new(transport: Arc<dyn Transport>) -> Self {
        Self {
            transport,
            hosts: Mutex::new(HashMap::new()),
            pacing: Mutex::new(HashMap::new()),
        }
    }

    pub async fn fetch(&self, value: &str, timeout: Duration) -> Result<Document, Error> {
        let deadline = Instant::now() + timeout;
        timeout_at(deadline, self.fetch_until(value, deadline))
            .await
            .map_err(|_| Error::timeout())?
    }

    async fn fetch_until(&self, value: &str, deadline: Instant) -> Result<Document, Error> {
        let mut url = validate_url(value)?;
        for _redirect in 0..6 {
            let hostname = url
                .host_str()
                .ok_or_else(|| Error::input("missing hostname"))?
                .trim_end_matches('.')
                .to_owned();
            let addresses = self.transport.resolve(&hostname).await?;
            let pinned_address = SocketAddr::new(select_address(addresses)?, 443);
            let host_lock = self
                .hosts
                .lock()
                .await
                .entry(hostname.clone())
                .or_default()
                .clone();
            let mut state = host_lock.lock().await;
            if state
                .blocked_until
                .is_some_and(|until| until > Instant::now())
            {
                return Err(Error::source(
                    "source_rate_limited",
                    "this source is rate limited and is not available right now",
                    true,
                ));
            }
            for attempt in 0..2 {
                self.pace(&hostname).await;
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(Error::timeout());
                }
                let response = self.transport.get(&url, pinned_address, remaining).await?;
                match response.status {
                    429 | 500 | 502 | 503 | 504 => {
                        let limited = response.status == 429;
                        let fallback = if limited { 3.0 } else { 2.0 };
                        let delay = retry_after(
                            response
                                .headers
                                .get(RETRY_AFTER)
                                .and_then(|value| value.to_str().ok()),
                            fallback,
                        );
                        if attempt == 0 {
                            sleep(delay).await;
                            continue;
                        }
                        let cooldown = delay.max(Duration::from_secs(if limited { 30 } else { 5 }));
                        state.blocked_until = Some(Instant::now() + cooldown);
                        return Err(Error::source(
                            if limited {
                                "source_rate_limited"
                            } else {
                                "source_unavailable"
                            },
                            format!("source returned HTTP {}", response.status),
                            true,
                        ));
                    }
                    301 | 302 | 303 | 307 | 308 => {
                        let location = response
                            .headers
                            .get(LOCATION)
                            .and_then(|value| value.to_str().ok())
                            .ok_or_else(|| {
                                Error::source(
                                    "invalid_redirect",
                                    "redirect has no destination",
                                    false,
                                )
                            })?;
                        if location.chars().any(char::is_control) || location.contains('\\') {
                            return Err(Error::input("redirect contains forbidden characters"));
                        }
                        let destination = url.join(location).map_err(|_| {
                            Error::source("invalid_redirect", "invalid redirect destination", false)
                        })?;
                        url = validate_url(destination.as_str())?;
                        break;
                    }
                    200 => {
                        if response.body.len() > MAX_RESPONSE_BYTES {
                            return Err(Error::too_large());
                        }
                        return Ok(Document {
                            final_url: url.to_string(),
                            content_type: response
                                .headers
                                .get(CONTENT_TYPE)
                                .and_then(|value| value.to_str().ok())
                                .unwrap_or_default()
                                .to_ascii_lowercase(),
                            body: response.body,
                        });
                    }
                    401 | 403 => {
                        return Err(Error::source(
                            "source_access_denied",
                            format!("source returned HTTP {}", response.status),
                            false,
                        ));
                    }
                    404 => {
                        return Err(Error::source(
                            "source_not_found",
                            "source returned HTTP 404",
                            false,
                        ));
                    }
                    status => {
                        return Err(Error::source(
                            "source_http_error",
                            format!("source returned HTTP {status}"),
                            false,
                        ));
                    }
                }
            }
        }
        Err(Error::input("source exceeded the redirect limit"))
    }

    async fn pace(&self, hostname: &str) {
        let (family, interval) = match hostname {
            "arxiv.org" | "export.arxiv.org" => ("arxiv", 3),
            "api.crossref.org" => ("crossref", 1),
            "api.openalex.org" => ("openalex", 1),
            _ => return,
        };
        let pacing_lock = self.pacing.lock().await.entry(family).or_default().clone();
        let mut last = pacing_lock.lock().await;
        if let Some(previous) = *last {
            sleep_until(previous + Duration::from_secs(interval)).await;
        }
        *last = Some(Instant::now());
    }
}

fn retry_after(value: Option<&str>, fallback: f64) -> Duration {
    let delay = value
        .and_then(|value| {
            if let Ok(seconds) = value.parse::<f64>() {
                return seconds.is_finite().then_some(seconds.clamp(0.0, 30.0));
            }
            httpdate::parse_http_date(value).ok().map(|when| {
                when.duration_since(SystemTime::now())
                    .unwrap_or_default()
                    .as_secs_f64()
                    .min(30.0)
            })
        })
        .unwrap_or(fallback);
    Duration::from_secs_f64(delay)
}

#[cfg(test)]
mod tests;
