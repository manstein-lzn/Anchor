use std::{ffi::OsString, path::PathBuf, time::Duration};

use clap::{Args, Parser, Subcommand};
use serde_json::Value;

use crate::{
    CitationRequest, Direction, Error, ReadManyRequest, ReadRequest, Scholarly, SearchRequest,
    Source, read_queries,
};

#[derive(Parser)]
#[command(
    name = "anchor-scholarly",
    about = "Search and read the literature; JSON on stdout, failures on stderr"
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Run the scholarly MCP server over newline-delimited JSON-RPC on stdio")]
    Mcp,
    Sources,
    Search {
        #[arg(long)]
        query: String,
        #[command(flatten)]
        search: SearchOptions,
    },
    SearchMany {
        #[arg(long)]
        queries_file: PathBuf,
        #[command(flatten)]
        search: SearchOptions,
        #[arg(long, default_value_t = 420, allow_hyphen_values = true)]
        budget: i64,
    },
    #[command(about = "Read an HTML, text or PDF document")]
    Read {
        #[arg(long)]
        url: String,
        #[command(flatten)]
        position: Position,
    },
    #[command(about = "Read up to eight documents, preserving per-document failures")]
    ReadMany {
        #[arg(long)]
        urls: String,
        #[command(flatten)]
        position: Position,
    },
    #[command(about = "Follow citations through OpenAlex")]
    Citations {
        #[arg(long)]
        identifier: String,
        #[arg(long, value_enum, default_value = "cited_by")]
        direction: Direction,
        #[arg(long, default_value_t = 8, allow_hyphen_values = true)]
        limit: i64,
    },
}

#[derive(Args)]
struct SearchOptions {
    #[arg(long, value_enum, default_value = "crossref")]
    source: Source,
    #[arg(long, default_value_t = 8, allow_hyphen_values = true)]
    limit: i64,
    #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
    offset: i64,
}

#[derive(Args)]
struct Position {
    #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
    offset: i64,
    #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
    page_start: i64,
}

impl Cli {
    pub fn try_parse_arguments(
        arguments: impl IntoIterator<Item = impl Into<OsString> + Clone>,
    ) -> Result<Self, clap::Error> {
        Self::try_parse_from(arguments)
    }

    pub fn is_mcp(&self) -> bool {
        matches!(self.command, Command::Mcp)
    }

    pub async fn execute(self, scholarly: &Scholarly) -> Result<Value, Error> {
        let timeout = Duration::from_secs(60);
        match self.command {
            Command::Mcp => Err(Error::input(
                "mcp is a stdio server and cannot produce JSON output",
            )),
            Command::Sources => Ok(scholarly.sources(timeout).await),
            Command::Search { query, search } => {
                scholarly
                    .search(
                        &SearchRequest {
                            query,
                            source: search.source,
                            limit: search.limit,
                            offset: search.offset,
                        },
                        timeout,
                    )
                    .await
            }
            Command::SearchMany {
                queries_file,
                search,
                budget,
            } => {
                scholarly
                    .search_many(
                        read_queries(&queries_file)?,
                        search.source,
                        search.limit,
                        search.offset,
                        budget,
                        timeout,
                    )
                    .await
            }
            Command::Read { url, position } => {
                scholarly
                    .read(
                        &ReadRequest {
                            url,
                            offset: position.offset,
                            page_start: position.page_start,
                        },
                        timeout,
                    )
                    .await
            }
            Command::ReadMany { urls, position } => {
                let urls = urls
                    .split(',')
                    .map(str::trim)
                    .filter(|url| !url.is_empty())
                    .map(str::to_owned)
                    .collect();
                scholarly
                    .read_many(
                        &ReadManyRequest {
                            urls,
                            offset: position.offset,
                            page_start: position.page_start,
                        },
                        timeout,
                    )
                    .await
            }
            Command::Citations {
                identifier,
                direction,
                limit,
            } => {
                scholarly
                    .citations(
                        &CitationRequest {
                            identifier,
                            direction,
                            limit,
                        },
                        timeout,
                    )
                    .await
            }
        }
    }
}
