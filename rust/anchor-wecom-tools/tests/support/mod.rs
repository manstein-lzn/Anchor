use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderValue, StatusCode, header::LOCATION},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Clone)]
pub struct Reply {
    pub status: StatusCode,
    pub body: String,
}

impl Reply {
    pub fn json(value: Value) -> Self {
        Self {
            status: StatusCode::OK,
            body: value.to_string(),
        }
    }
}

impl IntoResponse for Reply {
    fn into_response(self) -> Response {
        let mut response = (self.status, self.body).into_response();
        if self.status.is_redirection() {
            response.headers_mut().insert(
                LOCATION,
                HeaderValue::from_static("/cgi-bin/message/send?access_token=redirect-token"),
            );
        }
        response
    }
}

pub type RecordedRequest = (String, HashMap<String, String>, Option<Value>);

pub struct FixtureState {
    pub token_calls: AtomicUsize,
    pub sends: AtomicUsize,
    pub users: AtomicUsize,
    pub expires: Mutex<Value>,
    pub token_reply: Mutex<Option<Reply>>,
    pub send_reply: Mutex<Reply>,
    pub user_reply: Mutex<Reply>,
    pub requests: Mutex<Vec<RecordedRequest>>,
}

pub struct Fixture {
    pub url: String,
    pub state: Arc<FixtureState>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fixture {
    pub async fn start() -> Self {
        let state = Arc::new(FixtureState {
            token_calls: AtomicUsize::new(0),
            sends: AtomicUsize::new(0),
            users: AtomicUsize::new(0),
            expires: Mutex::new(json!(7200)),
            token_reply: Mutex::new(None),
            send_reply: Mutex::new(Reply::json(
                json!({"errcode":0,"errmsg":"ok","msgid":"fixture-message"}),
            )),
            user_reply: Mutex::new(Reply::json(
                json!({"errcode":0,"userid":"成员","name":"本地成员"}),
            )),
            requests: Mutex::new(Vec::new()),
        });
        let router = Router::new()
            .route("/cgi-bin/gettoken", get(token))
            .route("/cgi-bin/message/send", post(send))
            .route("/cgi-bin/user/get", get(user))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { url, state, task }
    }
}

async fn token(
    State(state): State<Arc<FixtureState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Reply {
    let count = state.token_calls.fetch_add(1, Ordering::SeqCst) + 1;
    state
        .requests
        .lock()
        .unwrap()
        .push(("token".into(), query, None));
    state
        .token_reply
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| {
            Reply::json(json!({
                "errcode":0, "access_token":format!("fixture-token-{count}"),
                "expires_in":state.expires.lock().unwrap().clone()
            }))
        })
}

async fn send(
    State(state): State<Arc<FixtureState>>,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<Value>,
) -> Reply {
    state.sends.fetch_add(1, Ordering::SeqCst);
    state
        .requests
        .lock()
        .unwrap()
        .push(("send".into(), query, Some(body)));
    state.send_reply.lock().unwrap().clone()
}

async fn user(
    State(state): State<Arc<FixtureState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Reply {
    state.users.fetch_add(1, Ordering::SeqCst);
    state
        .requests
        .lock()
        .unwrap()
        .push(("user".into(), query, None));
    state.user_reply.lock().unwrap().clone()
}
