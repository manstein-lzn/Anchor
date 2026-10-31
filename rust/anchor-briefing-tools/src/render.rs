use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

type RenderError = Box<dyn Error + Send + Sync>;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Report {
    pub(crate) issue_date: String,
    pub(crate) window_start: String,
    pub(crate) cutoff: String,
    pub(crate) intro: Vec<String>,
    pub(crate) sections: Vec<Section>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) coverage_note: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Section {
    pub(crate) category: String,
    pub(crate) title: String,
    pub(crate) published_date: String,
    pub(crate) event_date: String,
    pub(crate) paragraphs: Vec<String>,
    pub(crate) sources: Vec<Source>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Source {
    pub(crate) title: String,
    pub(crate) url: String,
}

impl Report {
    pub(crate) fn validate(&self) -> Result<(), RenderError> {
        let issue = NaiveDate::parse_from_str(&self.issue_date, "%Y-%m-%d")?;
        let start = shanghai_time(&self.window_start)?;
        let cutoff = shanghai_time(&self.cutoff)?;
        if start >= cutoff || cutoff.date_naive() != issue {
            return Err("report window must end on the issue date, after window_start".into());
        }
        if self.intro.len() > 3 {
            return Err("intro must contain at most three selected takeaways".into());
        }
        if self.sections.is_empty() && self.coverage_note.is_none() {
            return Err("an issue without reliable events must explain the coverage gap".into());
        }
        for text in &self.intro {
            nonempty(text, "intro")?;
        }
        if let Some(note) = &self.coverage_note {
            nonempty(note, "coverage_note")?;
        }
        for section in &self.sections {
            nonempty(&section.category, "category")?;
            nonempty(&section.title, "title")?;
            NaiveDate::parse_from_str(&section.published_date, "%Y-%m-%d")?;
            if section.event_date != "unknown" {
                NaiveDate::parse_from_str(&section.event_date, "%Y-%m-%d")?;
            }
            if section.paragraphs.is_empty() || section.sources.is_empty() {
                return Err(
                    "each selected event must contain paragraphs and direct sources".into(),
                );
            }
            for paragraph in &section.paragraphs {
                nonempty(paragraph, "paragraph")?;
            }
            for source in &section.sources {
                nonempty(&source.title, "source title")?;
                source_url(&source.url)?;
            }
        }
        Ok(())
    }

    fn template_data(&self) -> Result<Value, RenderError> {
        let issue = NaiveDate::parse_from_str(&self.issue_date, "%Y-%m-%d")?;
        let start = shanghai_time(&self.window_start)?;
        let cutoff = shanghai_time(&self.cutoff)?;
        let mut data = serde_json::to_value(self)?;
        data["issue_label"] = issue.format("%Y.%m.%d").to_string().into();
        data["weekday"] = issue.format("%a").to_string().to_uppercase().into();
        data["window_label"] = format!(
            "{} 至 {}",
            start.format("%Y.%m.%d %H:%M"),
            cutoff.format("%Y.%m.%d %H:%M")
        )
        .into();
        data["cutoff_label"] = cutoff.format("%Y.%m.%d %H:%M").to_string().into();
        for (section, data) in self.sections.iter().zip(
            data["sections"]
                .as_array_mut()
                .ok_or("sections must be an array")?,
        ) {
            let published = NaiveDate::parse_from_str(&section.published_date, "%Y-%m-%d")?;
            let mut label = format!("发表 {}月{}日", published.month(), published.day());
            if section.event_date != "unknown" && section.event_date != section.published_date {
                label.push_str(&format!(" · 事件 {}", section.event_date));
            }
            data["date_label"] = label.into();
            for source in data["sources"]
                .as_array_mut()
                .ok_or("sources must be an array")?
            {
                source["url"] =
                    source_url(source["url"].as_str().ok_or("source URL must be text")?)?
                        .to_string()
                        .into();
            }
        }
        Ok(data)
    }

    fn visible_text(&self) -> String {
        self.intro
            .iter()
            .chain(self.sections.iter().flat_map(|section| {
                [&section.category, &section.title]
                    .into_iter()
                    .chain(section.paragraphs.iter())
                    .chain(section.sources.iter().map(|source| &source.title))
            }))
            .chain(self.coverage_note.iter())
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub(crate) fn load_report(path: &Path) -> Result<Report, RenderError> {
    parse_report(&fs::read(path)?)
}

fn parse_report(bytes: &[u8]) -> Result<Report, RenderError> {
    let report: Report = serde_json::from_slice(bytes)?;
    report.validate()?;
    Ok(report)
}

fn nonempty(text: &str, field: &str) -> Result<(), RenderError> {
    if text.trim().is_empty() {
        return Err(format!("{field} must not be empty").into());
    }
    Ok(())
}

fn source_url(value: &str) -> Result<url::Url, RenderError> {
    let url = url::Url::parse(value)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("source URLs must use HTTP(S), without user information".into());
    }
    Ok(url)
}

fn shanghai_time(value: &str) -> Result<DateTime<FixedOffset>, RenderError> {
    let value = DateTime::parse_from_rfc3339(value)?;
    if value.offset().local_minus_utc() != 8 * 60 * 60 {
        return Err("report timestamps must use Asia/Shanghai (+08:00)".into());
    }
    Ok(value)
}

/// Render a selected report and expose files only after deterministic checks pass.
/// The caller owns workspace confinement and supplies already checked paths.
pub fn render(
    workspace: &Path,
    plugin_root: &Path,
    report: &Path,
    output_dir: &Path,
) -> Result<Value, RenderError> {
    let report_bytes = fs::read(report)?;
    let report = parse_report(&report_bytes)?;
    fs::create_dir_all(output_dir)?;
    let staging = tempfile::Builder::new()
        .prefix(".briefing-render-")
        .tempdir_in(output_dir)?;
    let root = staging.path();
    fs::copy(
        plugin_root.join("templates/morning.typ"),
        root.join("morning.typ"),
    )?;
    fs::write(
        root.join("report.json"),
        serde_json::to_vec(&report.template_data()?)?,
    )?;
    let stamp = report.issue_date.replace('-', "");
    let pdf_name = format!("AI每日晨报_{stamp}.pdf");
    let pdf = root.join(&pdf_name);
    let cutoff = shanghai_time(&report.cutoff)?;
    checked(
        Command::new(plugin_root.join("bin/typst"))
            .args(["compile", "--root"])
            .arg(root)
            .args(["--input", "report=report.json", "--ignore-system-fonts"])
            .args(["--font-path", "/usr/share/fonts/google-noto-cjk"])
            .args(["--font-path", "/usr/share/fonts/dejavu"])
            .args(["--creation-timestamp", &cutoff.timestamp().to_string()])
            .args(["--diagnostic-format", "short"])
            .arg(root.join("morning.typ"))
            .arg(&pdf),
        "Typst compilation",
    )?;
    let (inspection, text) = inspect_pdf(&report, &report_bytes, &pdf)?;
    let pages = inspection["pages"]
        .as_u64()
        .ok_or("inspection has no page count")? as usize;
    let text_name = format!("AI每日晨报_{stamp}.txt");
    fs::write(root.join(&text_name), &text)?;
    checked(
        Command::new("/usr/bin/pdftoppm")
            .args(["-scale-to", "1400", "-png"])
            .arg(&pdf)
            .arg(root.join("page")),
        "PDF page rendering",
    )?;
    let mut previews = fs::read_dir(root)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|path| {
            path.extension().is_some_and(|value| value == "png")
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("page-"))
        })
        .collect::<Vec<_>>();
    previews.sort_by_key(|path| {
        path.file_stem()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("page-"))
            .and_then(|number| number.parse::<usize>().ok())
            .unwrap_or(usize::MAX)
    });
    if previews.len() != pages
        || previews
            .iter()
            .any(|path| fs::metadata(path).map_or(true, |m| m.len() == 0))
    {
        return Err("PDF previews are missing or empty".into());
    }
    let preview_names = previews
        .iter()
        .map(|path| {
            path.file_name()
                .ok_or("preview has no filename")
                .map(PathBuf::from)
        })
        .collect::<Result<Vec<_>, _>>()?;
    fs::write(
        root.join("inspection.json"),
        serde_json::to_vec_pretty(&inspection)?,
    )?;
    for name in preview_names.iter().map(PathBuf::as_path).chain([
        Path::new(&pdf_name),
        Path::new(&text_name),
        Path::new("inspection.json"),
    ]) {
        fs::rename(root.join(name), output_dir.join(name))?;
    }
    Ok(json!({
        "pdf_path": relative(workspace, &output_dir.join(pdf_name))?,
        "pages": pages,
        "preview_paths": preview_names.iter().map(|name| relative(workspace, &output_dir.join(name))).collect::<Result<Vec<_>,_>>()?,
        "inspection_path": relative(workspace, &output_dir.join("inspection.json"))?,
        "text_path": relative(workspace, &output_dir.join(text_name))?,
    }))
}

/// Recheck the actual files at publication; an Agent-written inspection is evidence,
/// not the authority for whether a PDF satisfies the delivery contract.
pub(crate) fn verify_pdf(report_path: &Path, pdf_path: &Path) -> Result<Value, RenderError> {
    let bytes = fs::read(report_path)?;
    let report = parse_report(&bytes)?;
    Ok(inspect_pdf(&report, &bytes, pdf_path)?.0)
}

fn inspect_pdf(
    report: &Report,
    report_bytes: &[u8],
    pdf: &Path,
) -> Result<(Value, String), RenderError> {
    let text_output = checked(
        Command::new("/usr/bin/pdftotext")
            .arg("-layout")
            .arg(pdf)
            .arg("-"),
        "PDF text extraction",
    )?;
    let text = String::from_utf8(text_output.stdout)?;
    let fonts_output = checked(
        Command::new("/usr/bin/pdffonts").arg(pdf),
        "PDF font inspection",
    )?;
    let fonts = inspect_fonts(&String::from_utf8(fonts_output.stdout)?)?;
    let bbox_output = checked(
        Command::new("/usr/bin/pdftotext")
            .arg("-bbox-layout")
            .arg(pdf)
            .arg("-"),
        "PDF layout inspection",
    )?;
    let layout = inspect_layout(&String::from_utf8(bbox_output.stdout)?)?;
    let pages = layout.len();
    check_text(report, &text, pages)?;
    let links = inspect_links(pdf, report, pages)?;
    Ok((
        json!({
            "report_sha256": format!("{:x}", Sha256::digest(report_bytes)),
            "pdf_sha256": format!("{:x}", Sha256::digest(fs::read(pdf)?)),
            "static_checks_passed": true,
            "pages": pages,
            "fonts": fonts,
            "chinese_text_extraction": "passed",
            "page_numbers": "passed",
            "layout": layout,
            "link_count": links.iter().map(|link| link["annotation_count"].as_u64().unwrap_or(0)).sum::<u64>(),
            "links": links,
            "link_network_check": "not_performed; URI annotations checked against report sources",
            "visual_review": "required; inspect every page preview before publication",
        }),
        text,
    ))
}

fn relative(workspace: &Path, path: &Path) -> Result<String, RenderError> {
    Ok(path
        .strip_prefix(workspace)?
        .to_str()
        .ok_or("output path must be Unicode")?
        .to_owned())
}

fn checked(command: &mut Command, description: &str) -> Result<Output, RenderError> {
    let output = command.output()?;
    if !output.status.success() {
        return Err(format!(
            "{description} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(output)
}

fn inspect_fonts(output: &str) -> Result<Vec<Value>, RenderError> {
    let mut fonts = Vec::new();
    for line in output
        .lines()
        .skip(2)
        .filter(|line| !line.trim().is_empty())
    {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 8 {
            return Err("unrecognized pdffonts output".into());
        }
        let tail = &fields[fields.len() - 5..];
        if tail[0] != "yes" || tail[2] != "yes" {
            return Err(format!(
                "PDF font {} is not embedded with Unicode mapping",
                fields[0]
            )
            .into());
        }
        fonts.push(json!({"name": fields[0], "embedded": true, "unicode_mapping": true}));
    }
    if fonts.is_empty() {
        return Err("PDF contains no inspectable fonts".into());
    }
    Ok(fonts)
}

fn inspect_layout(xml: &str) -> Result<Vec<Value>, RenderError> {
    let document = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        },
    )?;
    let mut result = Vec::new();
    for (index, page) in document
        .descendants()
        .filter(|node| node.has_tag_name("page"))
        .enumerate()
    {
        let number = |node: roxmltree::Node<'_, '_>, name| -> Result<f64, RenderError> {
            Ok(node
                .attribute(name)
                .ok_or("missing PDF layout coordinate")?
                .parse()?)
        };
        let width = number(page, "width")?;
        let height = number(page, "height")?;
        if (width - 595.276).abs() > 1.0 || (height - 841.89).abs() > 1.0 {
            return Err("PDF must use A4 portrait pages".into());
        }
        let mut bounds = [width, height, 0.0_f64, 0.0_f64];
        let mut body_bounds = [width, height, 0.0_f64, 0.0_f64];
        let mut words = 0;
        let mut body_words = 0;
        for word in page.descendants().filter(|node| node.has_tag_name("word")) {
            let rect = [
                number(word, "xMin")?,
                number(word, "yMin")?,
                number(word, "xMax")?,
                number(word, "yMax")?,
            ];
            if rect.iter().any(|value| !value.is_finite())
                || rect[0] < 47.0
                || rect[2] > width - 47.0
                || rect[1] < 18.0
                || rect[3] > height - 18.0
            {
                return Err(format!(
                    "text extends outside the safe page boundary on page {}: {:?} at {rect:?}",
                    index + 1,
                    word.text().unwrap_or_default()
                )
                .into());
            }
            bounds[0] = bounds[0].min(rect[0]);
            bounds[1] = bounds[1].min(rect[1]);
            bounds[2] = bounds[2].max(rect[2]);
            bounds[3] = bounds[3].max(rect[3]);
            if rect[1] > 50.0 && rect[1] < height - 50.0 {
                if rect[1] < 64.0 || rect[3] > height - 54.0 {
                    return Err(format!(
                        "body text overlaps the header or footer region on page {}",
                        index + 1
                    )
                    .into());
                }
                body_bounds[0] = body_bounds[0].min(rect[0]);
                body_bounds[1] = body_bounds[1].min(rect[1]);
                body_bounds[2] = body_bounds[2].max(rect[2]);
                body_bounds[3] = body_bounds[3].max(rect[3]);
                body_words += 1;
            } else if rect[1] >= height - 50.0 {
                let text = word.text().unwrap_or_default();
                if !matches!(text, "AI" | "每日晨报" | "/") && text.parse::<usize>().is_err() {
                    return Err(format!(
                        "unexpected text in the footer region on page {}",
                        index + 1
                    )
                    .into());
                }
            }
            words += 1;
        }
        if words == 0 || body_words == 0 {
            return Err(format!("PDF page {} is blank", index + 1).into());
        }
        result.push(json!({"page": index+1, "width_pt":width, "height_pt":height, "text_bounds_pt": bounds, "body_bounds_pt": body_bounds, "word_count":words}));
    }
    if result.is_empty() {
        return Err("PDF has no pages".into());
    }
    Ok(result)
}

fn check_text(report: &Report, text: &str, pages: usize) -> Result<(), RenderError> {
    let cjk = |character: &char| matches!(*character as u32, 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x323af);
    let extracted = text.chars().filter(cjk).collect::<BTreeSet<_>>();
    let missing = report
        .visible_text()
        .chars()
        .filter(cjk)
        .filter(|character| !extracted.contains(character))
        .collect::<BTreeSet<_>>();
    if !missing.is_empty() || text.contains('\u{fffd}') {
        return Err(format!(
            "PDF Chinese text extraction lost characters: {}",
            missing.into_iter().collect::<String>()
        )
        .into());
    }
    let compact = |value: &str| {
        value
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>()
    };
    let text_pages = text
        .split('\u{000c}')
        .filter(|page| !page.trim().is_empty())
        .collect::<Vec<_>>();
    if text_pages.len() != pages {
        return Err("PDF text extraction page count differs from layout inspection".into());
    }
    // Running furniture sits between a paragraph's two halves when it crosses a
    // page, so compare manuscript text against the body rather than the full page.
    let body_text = text_pages
        .iter()
        .map(|page| {
            page.lines()
                .filter(|line| !line.trim().is_empty())
                .skip(1)
                .collect::<Vec<_>>()
                .split_last()
                .map_or_else(String::new, |(_, body)| body.join("\n"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let compact_text = compact(&body_text);
    for expected in report
        .visible_text()
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        if !compact_text.contains(&compact(expected)) {
            return Err(format!(
                "PDF text extraction does not contain the complete report text: {expected}"
            )
            .into());
        }
    }
    for (index, page) in text_pages.iter().enumerate() {
        let expected = format!("{:02}/{pages:02}", index + 1);
        if !compact(page).contains(&expected) {
            return Err(format!("missing page number {expected}").into());
        }
    }
    Ok(())
}

fn inspect_links(pdf: &Path, report: &Report, pages: usize) -> Result<Vec<Value>, RenderError> {
    let document = lopdf::Document::load(pdf)?;
    if document.get_pages().len() != pages {
        return Err("PDF parser page count differs from layout inspection".into());
    }
    let mut actual = BTreeMap::<String, usize>::new();
    for page_id in document.get_pages().values() {
        for annotation in document.get_page_annotations(*page_id)? {
            if annotation
                .get(b"Subtype")
                .and_then(lopdf::Object::as_name)
                .ok()
                != Some(b"Link")
            {
                continue;
            }
            let action = annotation.get_deref(b"A", &document)?.as_dict()?;
            let uri = action.get(b"URI")?.as_str()?;
            *actual
                .entry(source_url(&String::from_utf8(uri.to_vec())?)?.to_string())
                .or_default() += 1;
        }
    }
    let expected = report
        .sections
        .iter()
        .flat_map(|section| {
            section
                .sources
                .iter()
                .map(|source| source_url(&source.url).map(|url| url.to_string()))
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    for url in &expected {
        if !actual.contains_key(url) {
            return Err(format!("PDF is missing a clickable source: {url}").into());
        }
    }
    if actual.keys().any(|url| !expected.contains(url)) {
        return Err("PDF contains source URI annotations absent from the report".into());
    }
    Ok(actual
        .into_iter()
        .map(|(url, count)| json!({"url":url,"annotation_count":count}))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires ANCHOR_BRIEFING_TEST_TYPST and system Poppler tools"]
    fn renders_and_rechecks_real_pdf_without_requiring_news_quota() -> Result<(), RenderError> {
        let binary = std::env::var_os("ANCHOR_BRIEFING_TEST_TYPST")
            .ok_or("set ANCHOR_BRIEFING_TEST_TYPST to the approved Typst binary")?;
        let workspace = tempfile::tempdir()?;
        let plugin = tempfile::tempdir()?;
        fs::create_dir_all(plugin.path().join("bin"))?;
        fs::create_dir_all(plugin.path().join("templates"))?;
        fs::copy(binary, plugin.path().join("bin/typst"))?;
        fs::write(
            plugin.path().join("templates/morning.typ"),
            include_str!("../../../plugins/ai-morning-briefing/templates/morning.typ"),
        )?;
        let report_path = workspace.path().join("report.json");
        let mut report = json!({
            "issue_date":"2026-10-31",
            "window_start":"2026-10-30T16:07:00+08:00",
            "cutoff":"2026-10-31T09:00:00+08:00",
            "intro":["本文件用于中文排版验收，内容不作为真实新闻交付。"],
            "sections":[{
                "category":"排版验收",
                "title":"《中文排版》：核对字体、页码和可点击来源",
                "published_date":"2026-10-31",
                "event_date":"unknown",
                "paragraphs":["此段是确定性验证内容。PDF 中应保留原稿全部文字，字体应当嵌入并具有 Unicode 映射；直接来源必须成为可点击的 URI 注释。边界检查不得允许正文落入页眉和页脚。"],
                "sources":[{"title":"DeepSeek 官方文档", "url":"https://api-docs.deepseek.com/guides/search/"}]
            }]
        });
        fs::write(&report_path, serde_json::to_vec(&report)?)?;
        let delivery = render(
            workspace.path(),
            plugin.path(),
            &report_path,
            &workspace.path().join("delivery"),
        )?;
        let pdf = workspace
            .path()
            .join(delivery["pdf_path"].as_str().ok_or("missing PDF")?);
        let inspection = verify_pdf(&report_path, &pdf)?;
        assert_eq!(inspection["pages"], 1);
        assert!(
            inspection["link_count"]
                .as_u64()
                .is_some_and(|count| count > 0)
        );
        report["sections"][0]["paragraphs"] = json!([
            "这是用于跨页验证的长段落（包含中文标点与 English 字词）。正文必须自然延续到下一页，不能因为整条新闻保持在同一页而溢出，也不能因页眉和页脚插入而误判文字缺失。".repeat(30)
        ]);
        fs::write(&report_path, serde_json::to_vec(&report)?)?;
        let continued = render(
            workspace.path(),
            plugin.path(),
            &report_path,
            &workspace.path().join("continued"),
        )?;
        assert!(continued["pages"].as_u64().is_some_and(|pages| pages > 1));
        verify_pdf(
            &report_path,
            &workspace
                .path()
                .join(continued["pdf_path"].as_str().ok_or("missing PDF")?),
        )?;
        report["sections"] = json!([]);
        report["intro"] = json!([]);
        report["coverage_note"] = "本期未发现可核实的新进展，仍存在来源覆盖缺口。".into();
        fs::write(&report_path, serde_json::to_vec(&report)?)?;
        assert!(verify_pdf(&report_path, &pdf).is_err());
        let empty = render(
            workspace.path(),
            plugin.path(),
            &report_path,
            &workspace.path().join("short"),
        )?;
        assert_eq!(empty["pages"], 1);
        Ok(())
    }
}
