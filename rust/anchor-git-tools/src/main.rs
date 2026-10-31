use anchor_git_tools::{Config, Error, GitServer, credential_response};
use rmcp::ServiceExt;
use std::{
    io::{Read, Write},
    path::PathBuf,
};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let optional = |name| std::env::var(name).ok().filter(|s| !s.trim().is_empty());
    let config = Config {
        author_name: optional("ANCHOR_GIT_AUTHOR_NAME"),
        author_email: optional("ANCHOR_GIT_AUTHOR_EMAIL"),
        ssh_private_key: optional("ANCHOR_GIT_SSH_PRIVATE_KEY"),
        ssh_known_hosts: optional("ANCHOR_GIT_SSH_KNOWN_HOSTS"),
        ssh_config: optional("ANCHOR_GIT_SSH_CONFIG").map(PathBuf::from),
        https_url: optional("ANCHOR_GIT_HTTPS_URL"),
        https_username: optional("ANCHOR_GIT_HTTPS_USERNAME"),
        https_token: optional("ANCHOR_GIT_HTTPS_TOKEN"),
    };
    let mut arguments = std::env::args_os();
    let executable =
        PathBuf::from(arguments.next().ok_or("missing executable path")?).canonicalize()?;
    let arguments: Vec<_> = arguments.collect();
    if arguments.len() == 2 && arguments[0] == "--credential" {
        let mut input = String::new();
        std::io::stdin().take(65536).read_to_string(&mut input)?;
        let output = credential_response(&config, arguments[1].to_str().unwrap_or(""), &input)?;
        std::io::stdout().write_all(output.as_bytes())?;
        return Ok(());
    }
    if !arguments.is_empty() {
        return Err("usage: anchor-git-tools".into());
    }
    let workspace = std::env::var_os("ANCHOR_GIT_WORKSPACE")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    GitServer::new(workspace, config, executable)?
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await?;
    Ok(())
}
