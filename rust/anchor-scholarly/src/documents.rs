use std::{io, time::Duration};

use dom_smoothie::{Config, Readability, TextMode};
use encoding_rs::{Encoding, UTF_8};
use futures::future::join_all;
use serde_json::{Value, json};
use tokio::time::Instant;

use crate::{
    BATCH_READ_LIMIT, Error, MAX_RESPONSE_BYTES, PDF_PAGE_LIMIT, READ_CHARACTER_LIMIT,
    address::validate_url,
    fetch::{Document, Fetcher},
};

const MIN_ARTICLE_CHARACTERS: usize = 100;
const MAX_HTML_ELEMENTS: usize = 100_000;

#[derive(Clone, Debug)]
pub struct ReadRequest {
    pub url: String,
    pub offset: i64,
    pub page_start: i64,
}

impl ReadRequest {
    pub fn validate(&self) -> Result<(), Error> {
        validate_position(self.offset, self.page_start)?;
        if self.url.is_empty() || self.url.chars().count() > 3000 {
            return Err(Error::input("url must contain 1 to 3000 characters"));
        }
        validate_url(&self.url)?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ReadManyRequest {
    pub urls: Vec<String>,
    pub offset: i64,
    pub page_start: i64,
}

impl ReadManyRequest {
    pub fn validate(&self) -> Result<(), Error> {
        validate_position(self.offset, self.page_start)?;
        if self.urls.is_empty() {
            return Err(Error::input("scholarly.read_many requires urls"));
        }
        if self.urls.len() > BATCH_READ_LIMIT {
            return Err(Error::input("read-many accepts at most 8 URLs"));
        }
        Ok(())
    }
}

fn validate_position(offset: i64, page_start: i64) -> Result<(), Error> {
    if offset < 0 || page_start < 0 {
        return Err(Error::input("offset and page_start must be non-negative"));
    }
    Ok(())
}

pub(crate) async fn read(
    fetcher: &Fetcher,
    request: &ReadRequest,
    timeout: Duration,
) -> Result<Value, Error> {
    request.validate()?;
    let url = validate_url(&request.url)?;
    let arxiv = matches!(url.host_str(), Some("arxiv.org" | "www.arxiv.org"));
    if arxiv && url.path().starts_with("/abs/") {
        return Err(Error::source(
            "abstract_page",
            "arXiv /abs/ is the abstract page, not the paper; read https://arxiv.org/pdf/<id> for the full text",
            false,
        ));
    }
    let started = Instant::now();
    let document = if arxiv && url.path().starts_with("/pdf/") {
        let mut html_url = url.clone();
        let path = url.path().replacen("/pdf/", "/html/", 1);
        html_url.set_path(path.strip_suffix(".pdf").unwrap_or(&path));
        match fetcher.fetch(html_url.as_str(), timeout).await {
            Ok(document) => document,
            Err(error) if error.code != "invalid_request" => {
                fetcher
                    .fetch(&request.url, timeout.saturating_sub(started.elapsed()))
                    .await?
            }
            Err(error) => return Err(error),
        }
    } else {
        fetcher.fetch(&request.url, timeout).await?
    };
    let request = request.clone();
    tokio::task::spawn_blocking(move || extract(document, request))
        .await
        .map_err(|_| {
            Error::source(
                "source_extraction_failed",
                "document extraction failed",
                false,
            )
        })?
}

pub(crate) async fn read_many(
    fetcher: &Fetcher,
    request: &ReadManyRequest,
    timeout: Duration,
) -> Result<Value, Error> {
    request.validate()?;
    let documents = join_all(request.urls.iter().map(|url| async move {
        let single = ReadRequest {
            url: url.clone(),
            offset: request.offset,
            page_start: request.page_start,
        };
        match read(fetcher, &single, timeout).await {
            Ok(document) => document,
            Err(error) => json!({
                "requested_url": url, "url": null, "title": "", "content_type": null,
                "text": "", "error": error.summary(200), "evidence_available": false,
            }),
        }
    }))
    .await;
    let retrieved = documents
        .iter()
        .filter(|document| {
            document["text"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
        })
        .count();
    Ok(json!({"requested_urls": request.urls, "documents": documents, "retrieved": retrieved}))
}

struct Article {
    text: String,
    title: String,
    pages: Option<usize>,
}

fn extract(document: Document, request: ReadRequest) -> Result<Value, Error> {
    let article = if document.content_type.contains("application/pdf")
        || document.body.starts_with(b"%PDF-")
    {
        std::panic::catch_unwind(|| pdf_article(&document.body, request.page_start)).map_err(
            |_| {
                Error::source(
                    "unsupported_pdf",
                    "PDF content is corrupt or uses unsupported extraction features",
                    false,
                )
            },
        )??
    } else if document.content_type.contains("html")
        || document.body.trim_ascii_start().starts_with(b"<!")
        || document.body.trim_ascii_start().starts_with(b"<html")
    {
        html_article(&document)?
    } else if document.content_type.starts_with("text/plain") {
        Article {
            text: String::from_utf8_lossy(&document.body).into_owned(),
            title: String::new(),
            pages: None,
        }
    } else {
        return Err(Error::input(
            "source is not a supported HTML, PDF, or text document",
        ));
    };
    if article.text.len() > MAX_RESPONSE_BYTES {
        return Err(extraction_limit());
    }
    if article.text.trim().chars().count() < MIN_ARTICLE_CHARACTERS {
        return Err(Error::input(
            "source contains no extractable article text; it may require access or OCR",
        ));
    }
    let length = article.text.chars().count();
    let offset = usize::try_from(request.offset)
        .ok()
        .filter(|offset| *offset < length)
        .ok_or_else(|| Error::input("offset is beyond the extracted document text"))?;
    let text = article
        .text
        .chars()
        .skip(offset)
        .take(READ_CHARACTER_LIMIT)
        .collect::<String>();
    let end = offset + text.chars().count();
    let next_offset = (end < length).then_some(end);
    let next_page_start = article.pages.and_then(|pages| {
        let next = request.page_start as usize + PDF_PAGE_LIMIT;
        (next < pages).then_some(next)
    });
    let mut result = json!({
        "url": document.final_url, "requested_url": request.url, "title": article.title,
        "content_type": document.content_type, "text": text,
        "truncated": next_offset.is_some() || next_page_start.is_some() || offset > 0 || request.page_start > 0,
        "pages": article.pages, "page_start": request.page_start, "offset": request.offset,
        "next_offset": next_offset,
    });
    if let Some(next) = next_page_start {
        result["next_page_start"] = json!(next);
    }
    Ok(result)
}

fn html_article(document: &Document) -> Result<Article, Error> {
    let declared_encoding = document
        .content_type
        .parse::<mime::Mime>()
        .ok()
        .and_then(|value| {
            value
                .get_param(mime::CHARSET)
                .and_then(|charset| Encoding::for_label(charset.as_str().as_bytes()))
        });
    let (encoding, bom_size) =
        Encoding::for_bom(&document.body).unwrap_or((declared_encoding.unwrap_or(UTF_8), 0));
    let (html, _) = encoding.decode_without_bom_handling(&document.body[bom_size..]);
    let config = Config {
        max_elements_to_parse: MAX_HTML_ELEMENTS,
        text_mode: TextMode::Formatted,
        ..Config::default()
    };
    let mut readability =
        Readability::new(html.into_owned(), Some(&document.final_url), Some(config)).map_err(
            |error| match error {
                dom_smoothie::ReadabilityError::TooManyElements(..) => extraction_limit(),
                _ => Error::input(
                    "source contains no extractable article text; it may require access or OCR",
                ),
            },
        )?;
    let article = readability.parse().map_err(|_| {
        Error::input("source contains no extractable article text; it may require access or OCR")
    })?;
    Ok(Article {
        text: article.text_content.to_string(),
        title: article.title,
        pages: None,
    })
}

fn pdf_article(body: &[u8], page_start: i64) -> Result<Article, Error> {
    let document = lopdf::Document::load_mem(body).map_err(|_| invalid_pdf())?;
    if document.is_encrypted() {
        return Err(Error::source(
            "encrypted_pdf",
            "PDF is encrypted and requires a password",
            false,
        ));
    }
    let pages = document.get_pages();
    let page_start = usize::try_from(page_start)
        .ok()
        .filter(|start| *start < pages.len())
        .ok_or_else(|| Error::input("page_start is beyond the document"))?;
    let title = document
        .trailer
        .get(b"Info")
        .ok()
        .and_then(|info| document.dereference(info).ok())
        .and_then(|(_, info)| info.as_dict().ok())
        .and_then(|info| info.get(b"Title").ok())
        .map(lopdf::decode_text_string)
        .transpose()
        .map_err(|_| invalid_pdf())?
        .unwrap_or_default();
    let mut text = BoundedText::default();
    for (index, (&page_number, &page_id)) in pages
        .iter()
        .skip(page_start)
        .take(PDF_PAGE_LIMIT)
        .enumerate()
    {
        for stream in document.get_page_contents(page_id) {
            document
                .get_object(stream)
                .and_then(lopdf::Object::as_stream)
                .and_then(lopdf::Stream::decompressed_content)
                .map_err(|_| invalid_pdf())?;
        }
        if index > 0 {
            io::Write::write_all(&mut text, b"\n\n").map_err(|_| extraction_limit())?;
        }
        let result = pdf_extract::output_doc_page(
            &document,
            &mut pdf_extract::PlainTextOutput::new(&mut text as &mut dyn io::Write),
            page_number,
        );
        if text.exhausted {
            return Err(extraction_limit());
        }
        result.map_err(|_| invalid_pdf())?;
    }
    Ok(Article {
        text: String::from_utf8(text.bytes).map_err(|_| invalid_pdf())?,
        title,
        pages: Some(pages.len()),
    })
}

#[derive(Default)]
struct BoundedText {
    bytes: Vec<u8>,
    exhausted: bool,
}

impl io::Write for BoundedText {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_RESPONSE_BYTES.saturating_sub(self.bytes.len()) {
            self.exhausted = true;
            return Err(io::Error::other("extracted text exceeds the size limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn invalid_pdf() -> Error {
    Error::source("invalid_pdf", "PDF is corrupt or cannot be parsed", false)
}

fn extraction_limit() -> Error {
    Error::source(
        "extraction_too_large",
        "document exceeds the extraction size limit",
        false,
    )
}
