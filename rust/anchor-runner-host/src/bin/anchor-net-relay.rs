//! Loopback relay for node sandboxes that share no network with the host.
//!
//! Such a sandbox runs in its own network namespace, so its loopback is the only
//! address it can reach. This relay opens a loopback listener inside that
//! namespace and forwards every connection to the host bridge over a UNIX socket
//! — the socket is the single, host-authorized way out. The relay also supervises
//! the wrapped command so the sandbox still runs one process tree.
//!
//! Usage:
//! `anchor-net-relay --socket <path> --listen <ip:port> -- <command> [args…]`

use std::{net::SocketAddr, path::PathBuf, process::ExitCode};
use tokio::{
    io::copy_bidirectional,
    net::{TcpListener, UnixStream},
};

/// One relay invocation.
struct Relay {
    socket: PathBuf,
    listen: SocketAddr,
    command: Vec<String>,
}

fn parse(args: Vec<String>) -> Result<Relay, String> {
    let mut socket = None;
    let mut listen = None;
    let mut command = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--socket" => {
                let value = args.get(index + 1).ok_or("--socket requires a path")?;
                socket = Some(PathBuf::from(value));
                index += 2;
            }
            "--listen" => {
                let value = args.get(index + 1).ok_or("--listen requires an address")?;
                listen = Some(
                    value
                        .parse::<SocketAddr>()
                        .map_err(|error| format!("invalid --listen address `{value}`: {error}"))?,
                );
                index += 2;
            }
            "--" => {
                command = args[index + 1..].to_vec();
                break;
            }
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }
    let socket = socket.ok_or("--socket is required")?;
    let listen = listen.ok_or("--listen is required")?;
    if command.is_empty() {
        return Err("a command is required after `--`".into());
    }
    Ok(Relay {
        socket,
        listen,
        command,
    })
}

/// Forward every accepted loopback connection to the host bridge socket.
async fn forward(listener: TcpListener, socket: PathBuf) {
    loop {
        let Ok((mut inbound, _)) = listener.accept().await else {
            return;
        };
        let socket = socket.clone();
        tokio::spawn(async move {
            let Ok(mut outbound) = UnixStream::connect(&socket).await else {
                return;
            };
            let _ = copy_bidirectional(&mut inbound, &mut outbound).await;
        });
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let relay = match parse(std::env::args().skip(1).collect()) {
        Ok(relay) => relay,
        Err(error) => {
            eprintln!("anchor-net-relay: {error}");
            return ExitCode::from(2);
        }
    };
    let listener = match TcpListener::bind(relay.listen).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!(
                "anchor-net-relay: cannot listen on {}: {error}",
                relay.listen
            );
            return ExitCode::from(2);
        }
    };
    let socket = relay.socket.clone();
    let forwarding = tokio::spawn(forward(listener, socket));
    let mut child = match tokio::process::Command::new(&relay.command[0])
        .args(&relay.command[1..])
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            eprintln!(
                "anchor-net-relay: cannot start `{}`: {error}",
                relay.command[0]
            );
            forwarding.abort();
            return ExitCode::from(2);
        }
    };
    let status = child.wait().await;
    forwarding.abort();
    match status {
        Ok(status) => ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1)),
        Err(error) => {
            eprintln!("anchor-net-relay: wrapped command failed: {error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn parse_requires_a_socket_listener_and_command() {
        assert!(parse(args(&["--listen", "127.0.0.1:1", "--", "sh"])).is_err());
        assert!(parse(args(&["--socket", "/s", "--", "sh"])).is_err());
        assert!(parse(args(&["--socket", "/s", "--listen", "127.0.0.1:1"])).is_err());
        assert!(parse(args(&["--socket", "/s", "--listen", "nope", "--", "sh"])).is_err());
        assert!(
            parse(args(&[
                "--socket",
                "/s",
                "--listen",
                "127.0.0.1:1",
                "--",
                "sh"
            ]))
            .is_ok()
        );
    }

    #[test]
    fn parse_reads_the_wrapped_command_verbatim() {
        let relay = parse(args(&[
            "--socket",
            "/run/bridge.sock",
            "--listen",
            "127.0.0.1:9080",
            "--",
            "/tools/goose",
        ]))
        .unwrap();
        assert_eq!(relay.socket, PathBuf::from("/run/bridge.sock"));
        assert_eq!(relay.listen, "127.0.0.1:9080".parse().unwrap());
        assert_eq!(relay.command, ["/tools/goose"]);
    }

    #[tokio::test]
    async fn forwarded_connections_carry_bytes_both_ways() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("bridge.sock");
        let server = tokio::net::UnixListener::bind(&socket).unwrap();
        let upstream = tokio::spawn(async move {
            let (mut stream, _) = server.accept().await.unwrap();
            let mut request = [0u8; 64];
            let read = stream.read(&mut request).await.unwrap();
            assert_eq!(&request[..read], b"ping");
            stream.write_all(b"pong").await.unwrap();
        });

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let forwarding = tokio::spawn(forward(listener, socket));

        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        client.write_all(b"ping").await.unwrap();
        let mut response = [0u8; 4];
        client.read_exact(&mut response).await.unwrap();
        assert_eq!(&response, b"pong");
        upstream.await.unwrap();
        forwarding.abort();
    }
}
