use super::*;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixListener,
};

#[tokio::test]
async fn graph_listener_observes_supervised_route_without_exposing_control_credentials() {
    let (root, mut state) = fixture();
    let socket = root.path().join("control.sock");
    let descriptor = root.path().join("control.json");
    let token = "private-channel-control-token";
    std::fs::write(
        &descriptor,
        json!({"socket":socket,"token":token}).to_string(),
    )
    .unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    let gateway = tokio::spawn(async move {
        for status in [
            "authenticating",
            "authenticated",
            "reconnecting",
            "invalid-status",
        ] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            BufReader::new(&mut stream)
                .read_until(b'\n', &mut bytes)
                .await
                .unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(request, json!({"operation":"status","token":token}));
            stream
                .write_all(format!("{{\"status\":\"{status}\"}}\n").as_bytes())
                .await
                .unwrap();
        }
    });
    state
        .channel_descriptors
        .insert("wecom".into(), descriptor.clone());
    for expected in [
        "authenticating",
        "authenticated",
        "reconnecting",
        "unavailable",
    ] {
        let (status, projection) = call(router(state.clone()), "GET", "/graphs", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            projection["graphs"][0]["listener"],
            json!({"platform":"wecom","status":expected})
        );
        assert!(!projection.to_string().contains(token));
        assert!(!projection.to_string().contains("control.sock"));
    }
    gateway.await.unwrap();
    std::fs::remove_file(descriptor).unwrap();
    let (_, projection) = call(router(state.clone()), "GET", "/graphs", None).await;
    assert_eq!(projection["graphs"][0]["listener"]["status"], "unavailable");
    state.wecom.graph = "other-graph".into();
    let (_, projection) = call(router(state), "GET", "/graphs", None).await;
    assert!(projection["graphs"][0].get("listener").is_none());
}

#[tokio::test]
async fn graph_listener_is_absent_without_supervision() {
    let (_root, state) = fixture();
    let (_, projection) = call(router(state), "GET", "/graphs", None).await;
    assert!(projection["graphs"][0].get("listener").is_none());
}
