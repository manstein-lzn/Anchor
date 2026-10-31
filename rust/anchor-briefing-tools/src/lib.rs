#![forbid(unsafe_code)]
mod package;
mod render;
pub use package::package_plugin;

use anchor_file_lease::{FileLease, FlockOperation};
use anchor_workspace_files::WorkspaceFiles;
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, FixedOffset, NaiveDate, SecondsFormat, Utc};
use rmcp::{
    RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone)]
pub struct BriefingServer {
    workspace: PathBuf,
    plugin_root: PathBuf,
    files: WorkspaceFiles,
}

#[derive(Clone, Deserialize, Serialize)]
struct Publication {
    issue_id: String,
    issue_date: String,
    revision: u32,
    window_start: String,
    cutoff: String,
    published_at: String,
    report_sha256: Option<String>,
    pdf_sha256: String,
    imported_baseline: bool,
    visual_review: Option<VisualReview>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VisualReview {
    pub approved: bool,
    pub pages_reviewed: Vec<usize>,
    pub notes: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextRequest {
    #[serde(default = "context_dir")]
    pub output_dir: String,
}
fn context_dir() -> String {
    "context".into()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueRequest {
    #[serde(default)]
    pub issue_date: Option<String>,
    #[serde(default)]
    pub revision: Option<u32>,
    pub output_dir: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderRequest {
    pub report_path: String,
    #[serde(default = "delivery_dir")]
    pub output_dir: String,
}
fn delivery_dir() -> String {
    "delivery".into()
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublishRequest {
    pub report_path: String,
    pub pdf_path: String,
    pub inspection_path: String,
    pub visual_review: VisualReview,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewRequest {
    path: String,
}

impl BriefingServer {
    pub fn new(workspace: impl AsRef<Path>, plugin_root: impl AsRef<Path>) -> Result<Self, Error> {
        let workspace = workspace.as_ref().canonicalize()?;
        let plugin_root = plugin_root.as_ref().canonicalize()?;
        fs::create_dir_all(plugin_root.join("data/issues"))?;
        Ok(Self {
            files: WorkspaceFiles::new(&workspace)?,
            workspace,
            plugin_root,
        })
    }

    fn input(&self, value: &str) -> Result<PathBuf, Error> {
        let relative = WorkspaceFiles::validate_relative(value)?;
        let path = self.workspace.join(relative).canonicalize()?;
        if !path.starts_with(&self.workspace) || !path.is_file() {
            return Err("input must be a regular file inside the workspace; copy selected upstream files first".into());
        }
        Ok(path)
    }

    fn publications(&self) -> Result<Vec<(Publication, PathBuf)>, Error> {
        let mut publications = Vec::new();
        for day in fs::read_dir(self.plugin_root.join("data/issues"))? {
            let day = day?;
            if !day.file_type()?.is_dir() || day.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            for revision in fs::read_dir(day.path())? {
                let revision = revision?;
                if !revision.file_type()?.is_dir()
                    || revision.file_name().to_string_lossy().starts_with('.')
                {
                    continue;
                }
                let publication = serde_json::from_slice::<Publication>(&fs::read(
                    revision.path().join("publication.json"),
                )?)?;
                publications.push((publication, revision.path()));
            }
        }
        publications.sort_by(|a, b| {
            a.0.cutoff
                .cmp(&b.0.cutoff)
                .then(a.0.revision.cmp(&b.0.revision))
        });
        Ok(publications)
    }

    fn export_publication(
        &self,
        publication: &Publication,
        directory: &Path,
        prefix: &str,
    ) -> Result<Value, Error> {
        let mut exported = json!({"publication":publication});
        for (name, field) in [
            ("briefing.pdf", "pdf_path"),
            ("briefing.txt", "text_path"),
            ("report.json", "report_path"),
        ] {
            let source = directory.join(name);
            if source.is_file() {
                let path = format!("{prefix}/{name}");
                self.files.write_new(&path, &fs::read(source)?)?;
                exported[field] = path.into();
            }
        }
        Ok(exported)
    }

    pub fn context(&self, request: ContextRequest) -> Result<Value, Error> {
        WorkspaceFiles::validate_relative(&request.output_dir)?;
        let now = Utc::now().with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap());
        let now_text = now.to_rfc3339_opts(SecondsFormat::Secs, false);
        let date = now.format("%Y-%m-%d").to_string();
        let publications = self.publications()?;
        let latest = publications.last();
        let mut previous = Value::Null;
        let mut search_start = now
            .checked_sub_signed(chrono::Duration::days(1))
            .unwrap()
            .to_rfc3339();
        let mut issue_start = search_start.clone();
        if let Some((publication, directory)) = latest {
            search_start = publication.cutoff.clone();
            issue_start = if publication.issue_date == date {
                publication.window_start.clone()
            } else {
                search_start.clone()
            };
            previous = self.export_publication(
                publication,
                directory,
                &format!("{}/previous", request.output_dir),
            )?;
        }
        let previous_issue = match publications.iter().rev().find(|(p, _)| p.issue_date < date) {
            Some((publication, directory)) if latest.is_some_and(|(p, _)| p.issue_date == date) => {
                self.export_publication(
                    publication,
                    directory,
                    &format!("{}/previous-issue", request.output_dir),
                )?
            }
            Some(_) => previous.clone(),
            None => Value::Null,
        };
        let result = json!({"timezone":"Asia/Shanghai", "actual_now":now_text,
            "issue_date":date, "search_window_start":search_start, "issue_window_start":issue_start,
            "previous":previous, "previous_issue":previous_issue, "history_available":latest.is_some(),
            "history_gap": if latest.is_none() { Some("No previous published briefing is available; do not invent comparisons.") } else { None }});
        self.files.write_new(
            &format!("{}/context.json", request.output_dir),
            &serde_json::to_vec_pretty(&result)?,
        )?;
        Ok(result)
    }

    pub fn render(&self, request: RenderRequest) -> Result<Value, Error> {
        let report = self.input(&request.report_path)?;
        let relative = WorkspaceFiles::validate_relative(&request.output_dir)?;
        // Render into a fresh directory; old page images can never approve a new PDF.
        let destination = self.workspace.join(relative);
        let parent = destination
            .parent()
            .ok_or("output_dir has no parent")?
            .canonicalize()?;
        if !parent.starts_with(&self.workspace) || destination.exists() {
            return Err(
                "output_dir must be a new directory inside the workspace with an existing parent"
                    .into(),
            );
        }
        render::render(&self.workspace, &self.plugin_root, &report, &destination)
    }

    /// Export an exact published issue. A missing date never falls back to old news.
    pub fn issue(&self, request: IssueRequest) -> Result<Value, Error> {
        let issue_date = request.issue_date.unwrap_or_else(|| {
            Utc::now()
                .with_timezone(&FixedOffset::east_opt(28800).unwrap())
                .format("%Y-%m-%d")
                .to_string()
        });
        NaiveDate::parse_from_str(&issue_date, "%Y-%m-%d")?;
        WorkspaceFiles::validate_relative(&request.output_dir)?;
        let publications = self.publications()?;
        let (publication, directory) = publications
            .iter()
            .rev()
            .find(|(publication, _)| {
                publication.issue_date == issue_date
                    && request
                        .revision
                        .is_none_or(|revision| revision == publication.revision)
            })
            .ok_or("the requested published briefing is not available")?;
        let pdf = fs::read(directory.join("briefing.pdf"))?;
        if digest(&pdf) != publication.pdf_sha256 {
            return Err("archived PDF differs from its published identity".into());
        }
        let pdf_path = format!(
            "{}/AI每日晨报_{}.pdf",
            request.output_dir,
            issue_date.replace('-', "")
        );
        self.files.write_new(&pdf_path, &pdf)?;
        let mut result = json!({"publication":publication,"pdf_path":pdf_path});
        for (name, field) in [
            ("report.json", "report_path"),
            ("briefing.txt", "text_path"),
        ] {
            let path = directory.join(name);
            if path.is_file() {
                let bytes = fs::read(path)?;
                if name == "report.json"
                    && publication.report_sha256.as_deref() != Some(digest(&bytes).as_str())
                {
                    return Err("archived report differs from its published identity".into());
                }
                let target = format!("{}/{name}", request.output_dir);
                self.files.write_new(&target, &bytes)?;
                result[field] = target.into();
            }
        }
        let manifest = format!("{}/publication.json", request.output_dir);
        self.files
            .write_new(&manifest, &serde_json::to_vec_pretty(&result)?)?;
        result["publication_path"] = manifest.into();
        Ok(result)
    }

    pub fn publish(&self, request: PublishRequest) -> Result<Value, Error> {
        let report_path = self.input(&request.report_path)?;
        let report = render::load_report(&report_path)?;
        let report_value = serde_json::to_value(report)?;
        let pdf_path = self.input(&request.pdf_path)?;
        let inspection_path = self.input(&request.inspection_path)?;
        let report_bytes = fs::read(&report_path)?;
        let pdf_bytes = fs::read(&pdf_path)?;
        let inspection_bytes = fs::read(&inspection_path)?;
        let inspection: Value = serde_json::from_slice(&inspection_bytes)?;
        let report_sha = digest(&report_bytes);
        let pdf_sha = digest(&pdf_bytes);
        if inspection["static_checks_passed"] != true
            || inspection["report_sha256"] != report_sha
            || inspection["pdf_sha256"] != pdf_sha
        {
            return Err("inspection must pass and match this exact report and PDF".into());
        }
        let inspection = render::verify_pdf(&report_path, &pdf_path)?;
        let pages = inspection["pages"]
            .as_u64()
            .ok_or("inspection has no page count")? as usize;
        let mut reviewed = request.visual_review.pages_reviewed.clone();
        reviewed.sort_unstable();
        reviewed.dedup();
        if !request.visual_review.approved
            || request.visual_review.notes.trim().is_empty()
            || pages == 0
            || reviewed != (1..=pages).collect::<Vec<_>>()
        {
            return Err("visual review must approve and describe all actual PDF pages".into());
        }
        let cutoff = report_value["cutoff"]
            .as_str()
            .ok_or("report has no cutoff")?;
        if DateTime::parse_from_rfc3339(cutoff)?.with_timezone(&Utc) > Utc::now() {
            return Err("report cutoff cannot be in the future".into());
        }
        let _lease = self.publication_lease()?;
        let publications = self.publications()?;
        if let Some((existing, _)) = publications.iter().find(|(entry, _)| {
            entry.report_sha256.as_deref() == Some(&report_sha) && entry.pdf_sha256 == pdf_sha
        }) {
            return Ok(json!({"publication":existing, "changed":false}));
        }
        if publications
            .last()
            .is_some_and(|(latest, _)| latest.cutoff.as_str() > cutoff)
        {
            return Err(
                "a newer cutoff is already published; obtain current context before publishing"
                    .into(),
            );
        }
        let issue_date = report_value["issue_date"]
            .as_str()
            .ok_or("report has no issue_date")?;
        let revision = publications
            .iter()
            .filter(|(entry, _)| entry.issue_date == issue_date)
            .map(|(entry, _)| entry.revision)
            .max()
            .unwrap_or(0)
            + 1;
        let publication = Publication {
            issue_id: format!("ai-daily-{}", issue_date.replace('-', "")),
            issue_date: issue_date.into(),
            revision,
            window_start: report_value["window_start"]
                .as_str()
                .ok_or("report has no window_start")?
                .into(),
            cutoff: cutoff.into(),
            published_at: Utc::now().to_rfc3339(),
            report_sha256: Some(report_sha),
            pdf_sha256: pdf_sha,
            imported_baseline: false,
            visual_review: Some(request.visual_review),
        };
        self.archive(
            &publication,
            &[
                ("report.json", report_bytes),
                ("briefing.pdf", pdf_bytes),
                ("inspection.json", serde_json::to_vec_pretty(&inspection)?),
            ],
        )?;
        Ok(json!({"publication":publication,"changed":true}))
    }

    fn publication_lease(&self) -> Result<FileLease, Error> {
        let lock = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.plugin_root.join("data/publication.lock"))?;
        Ok(FileLease::acquire(
            lock,
            FlockOperation::NonBlockingLockExclusive,
        )?)
    }

    fn archive(&self, publication: &Publication, files: &[(&str, Vec<u8>)]) -> Result<(), Error> {
        let day = self
            .plugin_root
            .join("data/issues")
            .join(&publication.issue_date);
        fs::create_dir_all(&day)?;
        let staging = tempfile::Builder::new()
            .prefix(".publication-")
            .tempdir_in(&day)?;
        for (name, bytes) in files.iter().cloned().chain(std::iter::once((
            "publication.json",
            serde_json::to_vec_pretty(publication)?,
        ))) {
            let mut file = File::create(staging.path().join(name))?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        File::open(staging.path())?.sync_all()?;
        fs::rename(
            staging.path(),
            day.join(format!("{:04}", publication.revision)),
        )?;
        File::open(day)?.sync_all()?;
        Ok(())
    }

    pub fn seed_baseline(
        &self,
        pdf: &Path,
        issue_date: &str,
        cutoff: &str,
        window_start: &str,
    ) -> Result<Value, Error> {
        NaiveDate::parse_from_str(issue_date, "%Y-%m-%d")?;
        DateTime::parse_from_rfc3339(cutoff)?;
        let _lease = self.publication_lease()?;
        if !self.publications()?.is_empty() {
            return Err("baseline can only seed an empty briefing archive".into());
        }
        let text = Command::new("/usr/bin/pdftotext")
            .arg("-layout")
            .arg(pdf)
            .arg("-")
            .output()?;
        if !text.status.success() {
            return Err("cannot extract baseline PDF text".into());
        }
        let bytes = fs::read(pdf)?;
        let publication = Publication {
            issue_id: format!("ai-daily-{}", issue_date.replace('-', "")),
            issue_date: issue_date.into(),
            revision: 1,
            window_start: window_start.into(),
            cutoff: cutoff.into(),
            published_at: Utc::now().to_rfc3339(),
            report_sha256: None,
            pdf_sha256: digest(&bytes),
            imported_baseline: true,
            visual_review: None,
        };
        self.archive(
            &publication,
            &[("briefing.pdf", bytes), ("briefing.txt", text.stdout)],
        )?;
        Ok(serde_json::to_value(publication)?)
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn tool(name: &'static str, description: &'static str, properties: Value, required: Value) -> Tool {
    let schema = json!({"type":"object","properties":properties,"required":required,"additionalProperties":false});
    Tool::new(
        name,
        description,
        Arc::new(schema.as_object().unwrap().clone()),
    )
}

impl ServerHandler for BriefingServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("anchor-briefing", env!("CARGO_PKG_VERSION")),
        )
    }
    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: vec![
                tool(
                    "context",
                    "Read actual Asia/Shanghai time and export the latest revision (previous) and prior-date issue (previous_issue) into a NEW workspace directory. Call once to freeze this issue's observation cutoff; compare the prior-date issue even for same-day revisions.",
                    json!({"output_dir":{"type":"string","default":"context"}}),
                    json!([]),
                ),
                tool(
                    "issue",
                    "Export the exact published briefing for issue_date (defaults to actual Asia/Shanghai date) and optional revision into a new workspace directory. Verifies archived hashes; never substitutes a different date. Use the returned publication identity for delivery and discussion.",
                    json!({"issue_date":{"type":"string"},"revision":{"type":"integer","minimum":1},"output_dir":{"type":"string"}}),
                    json!(["output_dir"]),
                ),
                tool(
                    "render",
                    "Compile a typed morning report to a PDF and all page previews. Inspect actual returned pages before publication. Inputs must be workspace-relative; output_dir must be new.",
                    json!({"report_path":{"type":"string"},"output_dir":{"type":"string","default":"delivery"}}),
                    json!(["report_path"]),
                ),
                tool(
                    "preview",
                    "Return ONE actual rendered PNG page as an image, so its Chinese layout can be visually reviewed. Call for every preview_path returned by render.",
                    json!({"path":{"type":"string"}}),
                    json!(["path"]),
                ),
                tool(
                    "publish",
                    "Publish only this exact statically checked and visually approved PDF. Same-day editions keep issue identity and increment revision; the next run reads the last successful publication.",
                    json!({"report_path":{"type":"string"},"pdf_path":{"type":"string"},"inspection_path":{"type":"string"},"visual_review":{"type":"object","properties":{"approved":{"type":"boolean"},"pages_reviewed":{"type":"array","items":{"type":"integer","minimum":1}},"notes":{"type":"string"}},"required":["approved","pages_reviewed","notes"],"additionalProperties":false}}),
                    json!([
                        "report_path",
                        "pdf_path",
                        "inspection_path",
                        "visual_review"
                    ]),
                ),
            ],
            next_cursor: None,
            meta: None,
        })
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let arguments = json!(request.arguments.unwrap_or_default());
        if request.name == "preview" {
            let result = (|| -> Result<CallToolResult, Error> {
                let arguments: PreviewRequest = serde_json::from_value(arguments.clone())?;
                let path = self.input(&arguments.path)?;
                let bytes = fs::read(path)?;
                if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") || bytes.len() > 10 * 1024 * 1024 {
                    return Err("preview requires a rendered PNG no larger than 10 MiB".into());
                }
                Ok(CallToolResult::success(vec![ContentBlock::image(
                    STANDARD.encode(bytes),
                    "image/png",
                )]))
            })();
            return Ok(result.unwrap_or_else(|error| {
                CallToolResult::error(vec![ContentBlock::text(error.to_string())])
            }));
        }
        let result = match request.name.as_ref() {
            "context" => serde_json::from_value(arguments)
                .map_err(Error::from)
                .and_then(|args| self.context(args)),
            "issue" => serde_json::from_value(arguments)
                .map_err(Error::from)
                .and_then(|args| self.issue(args)),
            "render" => serde_json::from_value(arguments)
                .map_err(Error::from)
                .and_then(|args| self.render(args)),
            "publish" => serde_json::from_value(arguments)
                .map_err(Error::from)
                .and_then(|args| self.publish(args)),
            _ => return Err(ErrorData::invalid_params("unknown tool", None)),
        };
        match result {
            Ok(value) => {
                let mut result =
                    CallToolResult::success(vec![ContentBlock::text(value.to_string())]);
                result.structured_content = Some(value);
                Ok(result)
            }
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(
                error.to_string(),
            )])),
        }
    }
}

#[cfg(test)]
mod tests;
