#![allow(dead_code)]

use std::{
    collections::{HashMap, VecDeque},
    convert::Infallible,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use anchor_scholarly::{
    Error, Scholarly,
    transport::{HttpResponse, Transport, bounded_response},
};
use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CONTENT_TYPE, HOST, LOCATION},
    },
    response::Response,
};
use reqwest::Url;
use tokio::{net::TcpListener, sync::Mutex, task::JoinHandle, time::sleep};

pub const CROSSREF: &str = include_str!("../fixtures/crossref.json");
pub const OPENALEX: &str = include_str!("../fixtures/openalex.json");
pub const ARXIV: &str = include_str!("../fixtures/arxiv.xml");
pub const ARXIV_HTML: &str = include_str!("../fixtures/arxiv.html");

pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    pub delay_headers: Duration,
    pub delay_body: Duration,
    pub chunked: bool,
}

impl Reply {
    pub fn status(status: u16) -> Self {
        Self {
            status: StatusCode::from_u16(status).unwrap(),
            headers: HeaderMap::new(),
            body: Vec::new(),
            delay_headers: Duration::ZERO,
            delay_body: Duration::ZERO,
            chunked: false,
        }
    }

    pub fn body(value: impl AsRef<[u8]>) -> Self {
        let mut reply = Self::status(200);
        reply.body = value.as_ref().to_vec();
        reply
            .headers
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        reply
    }

    pub fn redirect(location: &str) -> Self {
        let mut reply = Self::status(302);
        reply
            .headers
            .insert(LOCATION, HeaderValue::from_str(location).unwrap());
        reply
    }
}

#[derive(Default)]
struct FixtureState {
    replies: Mutex<HashMap<(String, String), VecDeque<Reply>>>,
    requests: Mutex<Vec<Url>>,
}

pub struct LoopbackTransport {
    base: Url,
    client: reqwest::Client,
    pub pins: Mutex<Vec<SocketAddr>>,
}

#[async_trait]
impl Transport for LoopbackTransport {
    async fn resolve(&self, _hostname: &str) -> Result<Vec<IpAddr>, Error> {
        Ok(vec!["93.184.216.34".parse().unwrap()])
    }

    async fn get(
        &self,
        url: &Url,
        pinned_address: SocketAddr,
        timeout: Duration,
    ) -> Result<HttpResponse, Error> {
        self.pins.lock().await.push(pinned_address);
        let mut target = self.base.clone();
        target.set_path(url.path());
        target.set_query(url.query());
        let response = self
            .client
            .get(target)
            .header(HOST, url.host_str().unwrap())
            .timeout(timeout)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    Error::timeout()
                } else {
                    Error::source(
                        "source_unavailable",
                        "loopback fixture request failed",
                        true,
                    )
                }
            })?;
        bounded_response(response).await
    }
}

pub struct Fixture {
    state: Arc<FixtureState>,
    pub transport: Arc<LoopbackTransport>,
    server: JoinHandle<()>,
}

impl Fixture {
    pub async fn new() -> Self {
        let state = Arc::new(FixtureState::default());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new().fallback(handle).with_state(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .unwrap();
        let transport = Arc::new(LoopbackTransport {
            base: Url::parse(&format!("http://{address}")).unwrap(),
            client,
            pins: Mutex::new(Vec::new()),
        });
        Self {
            state,
            transport,
            server,
        }
    }

    pub fn scholarly(&self) -> Scholarly {
        Scholarly::with_transport(self.transport.clone())
    }

    pub async fn route(&self, hostname: &str, path: &str, replies: Vec<Reply>) {
        self.state
            .replies
            .lock()
            .await
            .insert((hostname.to_owned(), path.to_owned()), replies.into());
    }

    pub async fn requests(&self) -> Vec<Url> {
        self.state.requests.lock().await.clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn handle(State(state): State<Arc<FixtureState>>, request: Request) -> Response {
    let host = request
        .headers()
        .get(HOST)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let url = Url::parse(&format!("https://{host}{}", request.uri())).unwrap();
    state.requests.lock().await.push(url);
    let reply = state
        .replies
        .lock()
        .await
        .get_mut(&(host, request.uri().path().to_owned()))
        .and_then(VecDeque::pop_front)
        .unwrap_or_else(|| Reply::status(418));
    sleep(reply.delay_headers).await;
    let body = if reply.chunked || !reply.delay_body.is_zero() {
        Body::from_stream(futures::stream::once(async move {
            sleep(reply.delay_body).await;
            Ok::<_, Infallible>(reply.body)
        }))
    } else {
        Body::from(reply.body)
    };
    let mut response = Response::new(body);
    *response.status_mut() = reply.status;
    *response.headers_mut() = reply.headers;
    response
}
