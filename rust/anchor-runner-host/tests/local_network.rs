//! A node sandbox that shares no host network must still reach the bridge — and
//! nothing else. This is the counterpart of the bridge proxy: the sandbox talks
//! to an in-sandbox relay over its own loopback, the relay forwards to a UNIX
//! socket the host mounted read-only, and the host's loopback stays invisible.

use anchor_runtime::{NetworkPolicy, ReadOnlyInput, SandboxPort, SandboxRequest, SandboxStatus};
use anchor_sandbox_bwrap::{BubblewrapPolicy, BubblewrapSandbox};
use std::{path::Path, path::PathBuf, sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Port the relay listens on inside the sandbox.
const RELAY_PORT: u16 = 9080;
const BODY: &str = "relay-reached";

/// A host-side HTTP responder every request answers with `BODY`.
async fn host_responder() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request).await;
                let _ = stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{BODY}",
                            BODY.len()
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = stream.shutdown().await;
            });
        }
    });
    port
}

/// Forward UNIX-socket connections to the host responder.
fn forward_socket(socket: PathBuf, port: u16) {
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut inbound, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let Ok(mut outbound) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await
                else {
                    return;
                };
                let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
            });
        }
    });
}

fn request(
    workspace: &Path,
    socket_directory: &Path,
    relay: &ReadOnlyInput,
    command: &[String],
    network: NetworkPolicy,
) -> SandboxRequest {
    let mut request = SandboxRequest::new(workspace, command.to_vec());
    request.readonly_inputs = vec![
        // The relay enters the sandbox exactly like the packaged Goose binary:
        // an operator-authorized grant mounted at /tools/<name>.
        relay.clone(),
        // The host mounts the socket's directory read-only; the socket itself is
        // the only host service the sandbox can reach.
        ReadOnlyInput::new(socket_directory, "/in/host"),
    ];
    request.network = network;
    request.timeout = Duration::from_secs(20);
    request.max_output_bytes = 64 * 1024;
    request.cancellation = Arc::new(std::sync::atomic::AtomicBool::new(false));
    request
}

#[tokio::test]
async fn an_isolated_sandbox_reaches_the_host_only_through_the_relay() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let port = host_responder().await;
    let socket = directory.path().join("bridge.sock");
    forward_socket(socket.clone(), port);
    let relay = ReadOnlyInput::new(
        PathBuf::from(env!("CARGO_BIN_EXE_anchor-net-relay")),
        "/tools/anchor-net-relay",
    );
    let sandbox = BubblewrapSandbox::new(
        BubblewrapPolicy::new("bwrap", ["curl"])
            .authorize_workspace_root(&workspace)
            .authorize_readonly_input_root(directory.path())
            .authorize_readonly_destination_root("/in")
            // Only the control case below asks for the host network.
            .allow_network(),
    )
    .expect("these tests require working Bubblewrap isolation")
    .with_readonly_grants(std::slice::from_ref(&relay))
    .expect("the relay grant is host-authorized");

    // Isolated network: the sandbox reaches the host service through the relay.
    let relayed = sandbox
        .run(request(
            &workspace,
            directory.path(),
            &relay,
            &[
                "/tools/anchor-net-relay".to_owned(),
                "--socket".to_owned(),
                format!("/in/host/{}", socket.file_name().unwrap().to_string_lossy()),
                "--listen".to_owned(),
                format!("127.0.0.1:{RELAY_PORT}"),
                "--".to_owned(),
                "curl".to_owned(),
                "-s".to_owned(),
                "--max-time".to_owned(),
                "5".to_owned(),
                format!("http://127.0.0.1:{RELAY_PORT}/"),
            ],
            NetworkPolicy::Disabled,
        ))
        .await
        .unwrap();
    assert_eq!(relayed.status, SandboxStatus::Completed);
    assert!(relayed.stdout.contains(BODY), "{:?}", relayed.stdout);

    // ...and cannot reach the host's own loopback port that is listening.
    let direct = sandbox
        .run(request(
            &workspace,
            directory.path(),
            &relay,
            &[
                "curl".to_owned(),
                "-s".to_owned(),
                "--max-time".to_owned(),
                "3".to_owned(),
                format!("http://127.0.0.1:{port}/"),
            ],
            NetworkPolicy::Disabled,
        ))
        .await
        .unwrap();
    assert_ne!(direct.exit_code, Some(0), "{:?}", direct.stderr);
    assert!(!direct.stdout.contains(BODY), "{:?}", direct.stdout);

    // The control: sharing the host network is exactly what makes that port
    // reachable, so the negative above is not vacuous.
    let shared = sandbox
        .run(request(
            &workspace,
            directory.path(),
            &relay,
            &[
                "curl".to_owned(),
                "-s".to_owned(),
                "--max-time".to_owned(),
                "5".to_owned(),
                format!("http://127.0.0.1:{port}/"),
            ],
            NetworkPolicy::Enabled,
        ))
        .await
        .unwrap();
    assert!(shared.stdout.contains(BODY), "{:?}", shared.stdout);
}
