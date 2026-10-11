//! Optional Stirling-PDF REST backend (the homelab server on `go`) for tools
//! that need Tesseract / Ghostscript / qpdf / LibreOffice.
//!
//! Config: `$XDG_CONFIG_HOME/og-explorer/stirling.json`
//!   { "url": "https://go.tail6c6721.ts.net:8081", "api_key": "…" }
//! The key is sent as `X-API-KEY`. No config → `ServerConfig::load()` is
//! None and the UI hides server tools. All calls are blocking (run them on
//! the blocking pool).
//!
//! Endpoints (paths + multipart field names taken from the MIT-licensed
//! upstream sources, Stirling-Tools/Stirling-PDF `main`; class-level
//! prefixes come from app/common/.../annotations/api/{Misc,Convert,
//! Security,Info}Api.java = /api/v1/{misc,convert,security,info}).
//! C = app/core/src/main/java/stirling/software/SPDF/controller/api
//! M = app/core/src/main/java/stirling/software/SPDF/model/api
//!   GET  /api/v1/info/status            controller/web/MetricsController → {status, version}
//!   POST /api/v1/misc/ocr-pdf           C/misc/OCRController, M/misc/ProcessPdfWithOcrRequest
//!   POST /api/v1/misc/compress-pdf      C/misc/CompressController, M/misc/OptimizePdfRequest
//!   POST /api/v1/misc/repair            C/misc/RepairController (common PDFFile: fileInput)
//!   POST /api/v1/convert/pdf/pdfa       C/converters/ConvertPDFToPDFA, M/converters/PdfToPdfARequest
//!   POST /api/v1/convert/pdf/word       C/converters/ConvertPDFToOffice, M/converters/PdfToWordRequest
//!   POST /api/v1/convert/pdf/presentation  ConvertPDFToOffice, PdfToPresentationRequest
//!   POST /api/v1/convert/pdf/text       ConvertPDFToOffice, PdfToTextOrRTFRequest (txt|rtf)
//!   POST /api/v1/convert/pdf/xml        ConvertPDFToOffice (PDFFile)
//!   POST /api/v1/convert/pdf/html       C/converters/ConvertPDFToHtml (PDFFile)
//!   POST /api/v1/convert/pdf/markdown   M/converters/ConvertPDFToMarkdown (PDFFile)
//!   POST /api/v1/convert/pdf/xlsx       C/converters/ConvertPDFToExcelController, M/PDFWithPageNums
//!   POST /api/v1/convert/pdf/csv        C/converters/ExtractCSVController, M/PDFWithPageNums
//!   POST /api/v1/convert/pdf/epub       C/converters/ConvertPDFToEpubController, M/converters/ConvertPdfToEpubRequest
//!   POST /api/v1/convert/file/pdf       C/converters/ConvertOfficeController (common GeneralFile)
//!   POST /api/v1/convert/html/pdf       C/converters/ConvertHtmlToPDF (common HTMLToPdfRequest: zoom)
//!   POST /api/v1/convert/markdown/pdf   C/converters/ConvertMarkdownToPdf (GeneralFile)
//!   POST /api/v1/convert/eml/pdf        C/converters/ConvertEmlToPDF (common EmlToPdfRequest)
//!   POST /api/v1/convert/img/pdf        C/converters/ConvertImgPDFController, M/converters/ConvertToPdfRequest
//!   POST /api/v1/convert/svg/pdf        C/converters/ConvertSvgToPDF, M/converters/SvgToPdfRequest
//!   POST /api/v1/convert/ebook/pdf      C/converters/ConvertEbookToPDFController, M/converters/ConvertEbookToPdfRequest
//!   POST /api/v1/convert/cbz/pdf, /cbr/pdf  ConvertImgPDFController, Convert{Cbz,Cbr}ToPdfRequest
//!   POST /api/v1/convert/url/pdf        C/converters/ConvertWebsiteToPDF, M/converters/UrlToPdfRequest
//!   POST /api/v1/security/cert-sign     C/security/CertSignController, M/security/SignPDFWithCertRequest
//!   POST /api/v1/security/validate-signature  C/security/ValidateSignatureController, M/security/SignatureValidationRequest
//!   POST /api/v1/misc/auto-split-pdf    C/misc/AutoSplitPdfController, M/misc/AutoSplitPdfRequest
//!   POST /api/v1/misc/extract-image-scans  C/misc/ExtractImageScansController, M/misc/ExtractImageScansRequest

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerConfig {
    pub url: String,
    pub api_key: String,
}

impl ServerConfig {
    pub fn load() -> Option<ServerConfig> {
        let path = crate::config::config_dir().join("stirling.json");
        let s = std::fs::read_to_string(path).ok()?;
        let c: ServerConfig = serde_json::from_str(&s).ok()?;
        (!c.url.trim().is_empty()).then_some(c)
    }
}

/// A server result: the bytes plus the file extension the server says they
/// are (from Content-Type / Content-Disposition), e.g. "pdf", "docx", "zip".
#[derive(Debug, Clone)]
pub struct Reply {
    pub ext: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcrMode {
    /// skip pages that already have text
    Skip,
    /// OCR everything, replacing existing text layer
    Force,
}

/// Targets for PDF → other formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Docx,
    Odt,
    Xlsx,
    Pptx,
    Html,
    Markdown,
    Text,
    Rtf,
    Xml,
    Csv,
    Epub,
}

impl Target {
    pub const ALL: [Target; 11] = [
        Target::Docx,
        Target::Odt,
        Target::Xlsx,
        Target::Pptx,
        Target::Html,
        Target::Markdown,
        Target::Text,
        Target::Rtf,
        Target::Xml,
        Target::Csv,
        Target::Epub,
    ];
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Target::Docx => "Word (.docx)",
            Target::Odt => "OpenDocument (.odt)",
            Target::Xlsx => "Excel (.xlsx)",
            Target::Pptx => "PowerPoint (.pptx)",
            Target::Html => "HTML",
            Target::Markdown => "Markdown",
            Target::Text => "Plain text",
            Target::Rtf => "RTF",
            Target::Xml => "XML",
            Target::Csv => "CSV (tables)",
            Target::Epub => "EPUB",
        })
    }
}

/// Quick reachability + auth check for the settings UI.
pub fn ping(cfg: &ServerConfig) -> Result<String, String> {
    let url = join(&cfg.url, "/api/v1/info/status")?;
    let resp = agent(PING_TIMEOUT).get(&url).set("X-API-KEY", &cfg.api_key).call();
    let resp = check(resp)?;
    let body = read_body(resp)?;
    let json: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| "server answered, but it doesn't look like Stirling-PDF".to_string())?;
    let status = json.get("status").and_then(|v| v.as_str()).unwrap_or("UP");
    Ok(match json.get("version").and_then(|v| v.as_str()) {
        Some(v) if !v.is_empty() => format!("Stirling-PDF {v} ({status})"),
        _ => format!("Stirling-PDF ({status})"),
    })
}

pub fn ocr(cfg: &ServerConfig, pdf: &[u8], languages: &[String], mode: OcrMode) -> Result<Reply, String> {
    let mut form = Form::new();
    form.pdf(pdf);
    let langs: Vec<&str> = languages.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    if langs.is_empty() {
        form.text("languages", "eng");
    }
    for l in langs {
        form.text("languages", l);
    }
    form.text("ocrType", match mode {
        OcrMode::Skip => "skip-text",
        OcrMode::Force => "force-ocr",
    });
    form.text("ocrRenderType", "hocr");
    post(cfg, "/api/v1/misc/ocr-pdf", form, "pdf")
}

/// level 1 (light) … 9 (strongest, lossy images).
pub fn compress(cfg: &ServerConfig, pdf: &[u8], level: u8) -> Result<Reply, String> {
    let mut form = Form::new();
    form.pdf(pdf);
    form.text("optimizeLevel", &level.clamp(1, 9).to_string());
    form.text("linearize", "false");
    form.text("normalize", "false");
    form.text("grayscale", "false");
    post(cfg, "/api/v1/misc/compress-pdf", form, "pdf")
}

pub fn repair(cfg: &ServerConfig, pdf: &[u8]) -> Result<Reply, String> {
    let mut form = Form::new();
    form.pdf(pdf);
    post(cfg, "/api/v1/misc/repair", form, "pdf")
}

/// PDF/A-2b archival conversion.
pub fn to_pdfa(cfg: &ServerConfig, pdf: &[u8]) -> Result<Reply, String> {
    let mut form = Form::new();
    form.pdf(pdf);
    form.text("outputFormat", "pdfa-2b");
    post(cfg, "/api/v1/convert/pdf/pdfa", form, "pdf")
}

pub fn convert_from_pdf(cfg: &ServerConfig, pdf: &[u8], target: Target) -> Result<Reply, String> {
    let mut form = Form::new();
    form.pdf(pdf);
    let (path, fallback) = match target {
        Target::Docx => (form.with("outputFormat", "docx", "/api/v1/convert/pdf/word"), "docx"),
        Target::Odt => (form.with("outputFormat", "odt", "/api/v1/convert/pdf/word"), "odt"),
        Target::Pptx => (form.with("outputFormat", "pptx", "/api/v1/convert/pdf/presentation"), "pptx"),
        Target::Text => (form.with("outputFormat", "txt", "/api/v1/convert/pdf/text"), "txt"),
        Target::Rtf => (form.with("outputFormat", "rtf", "/api/v1/convert/pdf/text"), "rtf"),
        Target::Xlsx => (form.with("pageNumbers", "all", "/api/v1/convert/pdf/xlsx"), "xlsx"),
        Target::Csv => (form.with("pageNumbers", "all", "/api/v1/convert/pdf/csv"), "csv"),
        Target::Html => ("/api/v1/convert/pdf/html", "html"),
        Target::Markdown => ("/api/v1/convert/pdf/markdown", "md"),
        Target::Xml => ("/api/v1/convert/pdf/xml", "xml"),
        Target::Epub => {
            form.text("detectChapters", "true");
            form.text("targetDevice", "TABLET_PHONE_IMAGES");
            ("/api/v1/convert/pdf/epub", form.with("outputFormat", "EPUB", "epub"))
        }
    };
    post(cfg, path, form, fallback)
}

/// Any supported file (office documents, HTML, Markdown, EML, images…) to
/// PDF; the server picks the converter from `file_name`'s extension.
pub fn convert_to_pdf(cfg: &ServerConfig, file_name: &str, bytes: &[u8]) -> Result<Reply, String> {
    let ext = extension_of(file_name).ok_or("file has no extension, can't pick a converter")?;
    let name = safe_name(file_name, &ext);
    let mime = mime_for(&ext);
    let mut form = Form::new();
    form.file("fileInput", &name, mime, bytes);
    let path = match ext.as_str() {
        "html" | "htm" | "zip" => {
            form.text("zoom", "1");
            "/api/v1/convert/html/pdf"
        }
        "md" | "markdown" => "/api/v1/convert/markdown/pdf",
        "eml" => "/api/v1/convert/eml/pdf",
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "tif" | "tiff" | "webp" => {
            form.text("fitOption", "maintainAspectRatio");
            form.text("colorType", "color");
            form.text("autoRotate", "false");
            "/api/v1/convert/img/pdf"
        }
        "svg" => {
            form.text("combineIntoSinglePdf", "false");
            "/api/v1/convert/svg/pdf"
        }
        "epub" | "mobi" | "azw3" | "fb2" => {
            for k in ["embedAllFonts", "includeTableOfContents", "includePageNumbers", "optimizeForEbook"] {
                form.text(k, "false");
            }
            "/api/v1/convert/ebook/pdf"
        }
        "cbz" => "/api/v1/convert/cbz/pdf",
        "cbr" => "/api/v1/convert/cbr/pdf",
        _ => "/api/v1/convert/file/pdf",
    };
    post(cfg, path, form, "pdf")
}

pub fn url_to_pdf(cfg: &ServerConfig, url: &str) -> Result<Reply, String> {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) || url.len() < 10 {
        return Err("enter a full http:// or https:// address".into());
    }
    let mut form = Form::new();
    form.text("urlInput", url);
    post(cfg, "/api/v1/convert/url/pdf", form, "pdf")
}

/// Sign with a certificate (PKCS#12 bytes + password); visible signature
/// optional.
pub fn cert_sign(
    cfg: &ServerConfig,
    pdf: &[u8],
    p12: &[u8],
    password: &str,
    reason: &str,
    location: &str,
    visible: bool,
) -> Result<Reply, String> {
    if p12.is_empty() {
        return Err("no certificate file".into());
    }
    let mut form = Form::new();
    form.pdf(pdf);
    form.text("certType", "PKCS12");
    form.file("p12File", "certificate.p12", "application/x-pkcs12", p12);
    form.text("password", password);
    form.text("showSignature", if visible { "true" } else { "false" });
    form.text("showLogo", "false");
    if visible {
        form.text("pageNumber", "1");
    }
    if !reason.trim().is_empty() {
        form.text("reason", reason.trim());
    }
    if !location.trim().is_empty() {
        form.text("location", location.trim());
    }
    post(cfg, "/api/v1/security/cert-sign", form, "pdf")
}

/// Signature validation report (text).
pub fn validate_signature(cfg: &ServerConfig, pdf: &[u8]) -> Result<String, String> {
    let mut form = Form::new();
    form.pdf(pdf);
    let reply = post(cfg, "/api/v1/security/validate-signature", form, "json")?;
    Ok(signature_report(&reply.bytes))
}

/// Split at pages carrying a split QR code (Stirling auto-split); usually a
/// zip of PDFs.
pub fn auto_split(cfg: &ServerConfig, pdf: &[u8]) -> Result<Reply, String> {
    let mut form = Form::new();
    form.pdf(pdf);
    form.text("duplexMode", "false");
    post(cfg, "/api/v1/misc/auto-split-pdf", form, "zip")
}

/// Detect and split multiple photos from one scanned image (zip of images).
pub fn scanner_image_split(cfg: &ServerConfig, image_name: &str, image: &[u8]) -> Result<Reply, String> {
    let ext = extension_of(image_name).unwrap_or_else(|| "png".into());
    let mut form = Form::new();
    form.file("fileInput", &safe_name(image_name, &ext), mime_for(&ext), image);
    // primitive ints upstream: unsent fields would become 0, so send defaults
    for (k, v) in [("angleThreshold", "5"), ("tolerance", "20"), ("minArea", "8000"), ("minContourArea", "500"), ("borderSize", "1")] {
        form.text(k, v);
    }
    post(cfg, "/api/v1/misc/extract-image-scans", form, "zip")
}

// ---------------------------------------------------------------- transport

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// OCR / LibreOffice conversions of big files can take minutes.
const WORK_TIMEOUT: Duration = Duration::from_secs(600);
const PING_TIMEOUT: Duration = Duration::from_secs(15);

fn agent(read: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(CONNECT_TIMEOUT)
        .timeout_read(read)
        .timeout_write(read)
        // a redirect almost always means "login page"; report it instead
        .redirects(0)
        .user_agent(concat!("og-explorer/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// Base URL + absolute path; a bare host gets `https://`.
fn join(base: &str, path: &str) -> Result<String, String> {
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("no Stirling server URL configured".into());
    }
    let lower = base.to_ascii_lowercase();
    let base = if lower.starts_with("http://") || lower.starts_with("https://") {
        base.to_string()
    } else if lower.contains("://") {
        return Err("Stirling server URL must start with http:// or https://".into());
    } else {
        format!("https://{base}")
    };
    Ok(format!("{base}/{}", path.trim_start_matches('/')))
}

fn post(cfg: &ServerConfig, path: &str, form: Form, fallback_ext: &str) -> Result<Reply, String> {
    let url = join(&cfg.url, path)?;
    let (content_type, body) = form.finish();
    let resp = agent(WORK_TIMEOUT)
        .post(&url)
        .set("X-API-KEY", &cfg.api_key)
        .set("Content-Type", &content_type)
        .send_bytes(&body);
    let resp = check(resp)?;
    let ext = reply_ext(resp.header("Content-Disposition"), resp.header("Content-Type"))
        .unwrap_or_else(|| fallback_ext.to_string());
    let bytes = read_body(resp)?;
    if bytes.is_empty() {
        return Err("Stirling returned an empty file".into());
    }
    Ok(Reply { ext, bytes })
}

fn check(resp: Result<ureq::Response, ureq::Error>) -> Result<ureq::Response, String> {
    match resp {
        Ok(r) if (300..400).contains(&r.status()) => Err(format!(
            "server redirected ({}) — check the URL and API key (login page?)",
            r.status()
        )),
        Ok(r) => Ok(r),
        Err(ureq::Error::Status(code, r)) => {
            let snippet = r.into_string().map(|s| snippet(&s)).unwrap_or_default();
            Err(status_message(code, &snippet))
        }
        Err(ureq::Error::Transport(t)) => Err(format!("can't reach Stirling server ({t})")),
    }
}

fn status_message(code: u16, snippet: &str) -> String {
    match code {
        401 | 403 => "Stirling rejected the API key".into(),
        404 => "endpoint not available on this server version".into(),
        _ if snippet.is_empty() => format!("Stirling error {code}"),
        _ => format!("Stirling error {code}: {snippet}"),
    }
}

/// First ~200 chars of an error body, preferring a JSON `message`/`error`.
fn snippet(body: &str) -> String {
    let text = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|j| {
            ["message", "error", "detail"]
                .iter()
                .find_map(|k| j.get(k).and_then(|v| v.as_str()).map(str::to_string))
        })
        .unwrap_or_else(|| body.to_string());
    let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > 200 {
        format!("{}…", one_line.chars().take(200).collect::<String>())
    } else {
        one_line
    }
}

fn read_body(resp: ureq::Response) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    resp.into_reader()
        .read_to_end(&mut buf)
        .map_err(|e| format!("connection to Stirling dropped ({e})"))?;
    Ok(buf)
}

/// Extension from Content-Disposition's filename, else from Content-Type.
fn reply_ext(disposition: Option<&str>, content_type: Option<&str>) -> Option<String> {
    disposition
        .and_then(disposition_filename)
        .and_then(|f| extension_of(&f))
        .or_else(|| content_type.and_then(ext_for_mime).map(str::to_string))
}

fn disposition_filename(h: &str) -> Option<String> {
    let mut plain = None;
    for part in h.split(';').map(str::trim) {
        let Some((k, v)) = part.split_once('=') else { continue };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim();
        if k == "filename*" {
            // RFC 5987: charset'lang'percent-encoded
            let enc = v.splitn(3, '\'').nth(2).unwrap_or(v);
            return Some(percent_decode(enc.trim_matches('"')));
        }
        if k == "filename" {
            plain = Some(v.trim_matches('"').to_string());
        }
    }
    plain
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Lower-case extension (1–8 alphanumerics) of a file name.
fn extension_of(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let (stem, ext) = base.rsplit_once('.')?;
    (!stem.is_empty() && (1..=8).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()))
        .then(|| ext.to_ascii_lowercase())
}

fn ext_for_mime(ct: &str) -> Option<&'static str> {
    let ct = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    Some(match ct.as_str() {
        "application/pdf" => "pdf",
        "application/zip" | "application/x-zip-compressed" | "application/x-zip" => "zip",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        "application/msword" => "doc",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.oasis.opendocument.text" => "odt",
        "application/vnd.oasis.opendocument.spreadsheet" => "ods",
        "application/vnd.oasis.opendocument.presentation" => "odp",
        "application/epub+zip" => "epub",
        "application/rtf" | "text/rtf" => "rtf",
        "text/html" => "html",
        "text/markdown" | "text/x-markdown" => "md",
        "text/plain" => "txt",
        "text/csv" => "csv",
        "application/xml" | "text/xml" => "xml",
        "application/json" => "json",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/tiff" => "tiff",
        _ => return None,
    })
}

fn mime_for(ext: &str) -> &'static str {
    match ext {
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "rtf" => "application/rtf",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "md" | "markdown" => "text/markdown",
        "eml" => "message/rfc822",
        "zip" | "cbz" => "application/zip",
        "epub" => "application/epub+zip",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

/// Base name usable in a multipart header (no path, quotes or newlines).
fn safe_name(name: &str, ext: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let clean: String = base.chars().filter(|c| !c.is_control() && *c != '"').collect();
    if clean.trim().is_empty() { format!("input.{ext}") } else { clean }
}

/// Human-readable text from the validate-signature JSON (a list of result
/// objects); anything unexpected is shown pretty-printed.
fn signature_report(body: &[u8]) -> String {
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(body) else {
        return String::from_utf8_lossy(body).into_owned();
    };
    let Some(list) = json.as_array() else {
        return serde_json::to_string_pretty(&json).unwrap_or_default();
    };
    if list.is_empty() {
        return "No digital signatures found.".into();
    }
    let mut out = String::new();
    for (i, sig) in list.iter().enumerate() {
        out.push_str(&format!("Signature {}\n", i + 1));
        match sig.as_object() {
            Some(map) => {
                for (k, v) in map {
                    let v = match v {
                        serde_json::Value::Null => continue,
                        serde_json::Value::String(s) if s.is_empty() => continue,
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    out.push_str(&format!("  {k}: {v}\n"));
                }
            }
            None => out.push_str(&format!("  {sig}\n")),
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

// ---------------------------------------------------------------- multipart

struct Form {
    boundary: String,
    body: Vec<u8>,
}

impl Form {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mix = (nanos ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15)) ^ ((std::process::id() as u64) << 32);
        Form { boundary: format!("----ogExplorer{mix:016x}{n:08x}"), body: Vec::new() }
    }

    fn text(&mut self, name: &str, value: &str) {
        self.body.extend_from_slice(
            format!("--{}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n", self.boundary).as_bytes(),
        );
        self.body.extend_from_slice(value.as_bytes());
        self.body.extend_from_slice(b"\r\n");
    }

    fn file(&mut self, name: &str, file_name: &str, content_type: &str, bytes: &[u8]) {
        self.body.extend_from_slice(
            format!(
                "--{}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{file_name}\"\r\nContent-Type: {content_type}\r\n\r\n",
                self.boundary
            )
            .as_bytes(),
        );
        self.body.extend_from_slice(bytes);
        self.body.extend_from_slice(b"\r\n");
    }

    fn pdf(&mut self, pdf: &[u8]) {
        self.file("fileInput", "document.pdf", "application/pdf", pdf);
    }

    /// Add a text field and pass `ret` through (keeps match arms compact).
    fn with<'a>(&mut self, name: &str, value: &str, ret: &'a str) -> &'a str {
        self.text(name, value);
        ret
    }

    fn finish(mut self) -> (String, Vec<u8>) {
        self.body.extend_from_slice(format!("--{}--\r\n", self.boundary).as_bytes());
        (format!("multipart/form-data; boundary={}", self.boundary), self.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::mpsc;

    struct Captured {
        head: String,
        body: Vec<u8>,
    }

    impl Captured {
        fn header(&self, name: &str) -> Option<String> {
            self.head.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_string())
            })
        }
        fn body_str(&self) -> String {
            String::from_utf8_lossy(&self.body).into_owned()
        }
    }

    /// One-shot HTTP server on 127.0.0.1: captures the request, answers
    /// with `status`, extra `headers` and `body`.
    fn mock(status: &str, headers: &[(&str, &str)], body: &[u8]) -> (ServerConfig, mpsc::Receiver<Captured>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        let mut response = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
        for (k, v) in headers {
            response.push_str(&format!("{k}: {v}\r\n"));
        }
        response.push_str("\r\n");
        let mut response = response.into_bytes();
        response.extend_from_slice(body);
        std::thread::spawn(move || {
            let Ok((mut s, _)) = listener.accept() else { return };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            let head_end = loop {
                let n = s.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break p + 4;
                }
            };
            let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
            let len = head
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length").then(|| v.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            while buf.len() < head_end + len {
                let n = s.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let body = buf[head_end..].to_vec();
            let _ = s.write_all(&response);
            let _ = s.flush();
            let _ = tx.send(Captured { head, body });
        });
        (ServerConfig { url: format!("http://127.0.0.1:{port}/"), api_key: "secret-key".into() }, rx)
    }

    #[test]
    fn ocr_sends_multipart_and_key() {
        let (cfg, rx) = mock("200 OK", &[("Content-Type", "application/pdf"), ("Content-Disposition", "attachment; filename=\"document_ocr.pdf\"")], b"%PDF-1.7 ocr");
        let langs = vec!["eng".to_string(), "deu".to_string()];
        let reply = ocr(&cfg, b"%PDF-1.4 fake", &langs, OcrMode::Force).unwrap();
        assert_eq!(reply.ext, "pdf");
        assert_eq!(reply.bytes, b"%PDF-1.7 ocr");
        let req = rx.recv().unwrap();
        assert!(req.head.starts_with("POST /api/v1/misc/ocr-pdf HTTP/1.1"), "{}", req.head);
        assert_eq!(req.header("x-api-key").as_deref(), Some("secret-key"));
        let ct = req.header("content-type").unwrap();
        let boundary = ct.split("boundary=").nth(1).unwrap().to_string();
        assert!(ct.starts_with("multipart/form-data"));
        let body = req.body_str();
        assert!(body.contains("Content-Disposition: form-data; name=\"fileInput\"; filename=\"document.pdf\"\r\nContent-Type: application/pdf\r\n\r\n%PDF-1.4 fake\r\n"));
        assert!(body.contains("name=\"languages\"\r\n\r\neng\r\n"));
        assert!(body.contains("name=\"languages\"\r\n\r\ndeu\r\n"));
        assert!(body.contains("name=\"ocrType\"\r\n\r\nforce-ocr\r\n"));
        assert!(body.contains("name=\"ocrRenderType\"\r\n\r\nhocr\r\n"));
        assert!(body.ends_with(&format!("--{boundary}--\r\n")));
    }

    #[test]
    fn convert_from_pdf_picks_endpoint_and_ext() {
        let (cfg, rx) = mock(
            "200 OK",
            &[("Content-Type", "application/vnd.openxmlformats-officedocument.wordprocessingml.document")],
            b"PK..",
        );
        let reply = convert_from_pdf(&cfg, b"%PDF", Target::Docx).unwrap();
        assert_eq!(reply.ext, "docx");
        let req = rx.recv().unwrap();
        assert!(req.head.starts_with("POST /api/v1/convert/pdf/word "));
        assert!(req.body_str().contains("name=\"outputFormat\"\r\n\r\ndocx\r\n"));

        let (cfg, rx) = mock("200 OK", &[("Content-Type", "application/zip")], b"PK");
        assert_eq!(convert_from_pdf(&cfg, b"%PDF", Target::Epub).unwrap().ext, "zip");
        let body = rx.recv().unwrap().body_str();
        assert!(body.contains("name=\"outputFormat\"\r\n\r\nEPUB\r\n"));
        assert!(body.contains("name=\"detectChapters\"\r\n\r\ntrue\r\n"));
    }

    #[test]
    fn every_target_has_an_endpoint() {
        for t in Target::ALL {
            let (cfg, rx) = mock("200 OK", &[], b"x");
            let reply = convert_from_pdf(&cfg, b"%PDF", t).unwrap();
            let head = rx.recv().unwrap().head;
            assert!(head.starts_with("POST /api/v1/convert/pdf/"), "{t}: {head}");
            // no headers → per-target fallback extension
            assert!(!reply.ext.is_empty() && reply.ext != "pdf", "{t}");
        }
        let (cfg, rx) = mock("200 OK", &[], b"x");
        ocr(&cfg, b"%PDF", &[], OcrMode::Skip).unwrap();
        let body = rx.recv().unwrap().body_str();
        assert!(body.contains("name=\"ocrType\"\r\n\r\nskip-text\r\n"));
        assert!(body.contains("name=\"languages\"\r\n\r\neng\r\n"));
        // contract fn, reads the user's config file; just make sure it links
        let _load: fn() -> Option<ServerConfig> = ServerConfig::load;
    }

    #[test]
    fn convert_to_pdf_routes_by_extension() {
        for (name, path, mime) in [
            ("report.docx", "/api/v1/convert/file/pdf", "application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
            ("page.HTML", "/api/v1/convert/html/pdf", "text/html"),
            ("notes.md", "/api/v1/convert/markdown/pdf", "text/markdown"),
            ("mail.eml", "/api/v1/convert/eml/pdf", "message/rfc822"),
            ("photo.jpg", "/api/v1/convert/img/pdf", "image/jpeg"),
        ] {
            let (cfg, rx) = mock("200 OK", &[("Content-Type", "application/pdf")], b"%PDF");
            let reply = convert_to_pdf(&cfg, &format!("/home/x/{name}"), b"data").unwrap();
            assert_eq!(reply.ext, "pdf");
            let req = rx.recv().unwrap();
            assert!(req.head.starts_with(&format!("POST {path} ")), "{name}: {}", req.head);
            assert!(req.body_str().contains(&format!("filename=\"{name}\"\r\nContent-Type: {mime}\r\n")), "{name}");
        }
        let cfg = ServerConfig { url: "http://127.0.0.1:1".into(), api_key: String::new() };
        assert!(convert_to_pdf(&cfg, "noext", b"x").is_err());
    }

    #[test]
    fn cert_sign_fields() {
        let (cfg, rx) = mock("200 OK", &[("Content-Type", "application/pdf")], b"%PDF signed");
        cert_sign(&cfg, b"%PDF", b"P12DATA", "pw", "Approved", "Berlin", true).unwrap();
        let req = rx.recv().unwrap();
        assert!(req.head.starts_with("POST /api/v1/security/cert-sign "));
        let body = req.body_str();
        for needle in [
            "name=\"certType\"\r\n\r\nPKCS12\r\n",
            "name=\"p12File\"; filename=\"certificate.p12\"\r\nContent-Type: application/x-pkcs12\r\n\r\nP12DATA\r\n",
            "name=\"password\"\r\n\r\npw\r\n",
            "name=\"showSignature\"\r\n\r\ntrue\r\n",
            "name=\"reason\"\r\n\r\nApproved\r\n",
            "name=\"location\"\r\n\r\nBerlin\r\n",
        ] {
            assert!(body.contains(needle), "{needle}");
        }
    }

    #[test]
    fn ping_reports_version() {
        let (cfg, rx) = mock("200 OK", &[("Content-Type", "application/json")], br#"{"status":"UP","version":"1.4.0"}"#);
        assert_eq!(ping(&cfg).unwrap(), "Stirling-PDF 1.4.0 (UP)");
        let req = rx.recv().unwrap();
        assert!(req.head.starts_with("GET /api/v1/info/status "));
        assert_eq!(req.header("X-API-KEY").as_deref(), Some("secret-key"));
        let (cfg, _rx) = mock("200 OK", &[("Content-Type", "text/html")], b"<html>nginx</html>");
        assert!(ping(&cfg).is_err());
    }

    #[test]
    fn error_statuses_map_to_messages() {
        let (cfg, _rx) = mock("401 Unauthorized", &[], b"nope");
        assert_eq!(repair(&cfg, b"%PDF").unwrap_err(), "Stirling rejected the API key");
        let (cfg, _rx) = mock("403 Forbidden", &[], b"");
        assert_eq!(ping(&cfg).unwrap_err(), "Stirling rejected the API key");
        let (cfg, _rx) = mock("404 Not Found", &[], b"");
        assert_eq!(to_pdfa(&cfg, b"%PDF").unwrap_err(), "endpoint not available on this server version");
        let (cfg, _rx) = mock("500 Internal Server Error", &[("Content-Type", "application/json")], br#"{"message":"Ghostscript   failed\n badly"}"#);
        assert_eq!(compress(&cfg, b"%PDF", 5).unwrap_err(), "Stirling error 500: Ghostscript failed badly");
        let (cfg, _rx) = mock("302 Found", &[("Location", "/login")], b"");
        assert!(auto_split(&cfg, b"%PDF").unwrap_err().contains("redirected"));
        // nothing listening
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let cfg = ServerConfig { url: format!("http://127.0.0.1:{port}"), api_key: String::new() };
        assert!(repair(&cfg, b"%PDF").unwrap_err().starts_with("can't reach Stirling server"));
    }

    #[test]
    fn validate_signature_formats_report() {
        let (cfg, _rx) = mock(
            "200 OK",
            &[("Content-Type", "application/json")],
            br#"[{"valid":true,"signerName":"Alice","reason":"","location":null}]"#,
        );
        let r = validate_signature(&cfg, b"%PDF").unwrap();
        assert!(r.starts_with("Signature 1"));
        assert!(r.contains("signerName: Alice") && r.contains("valid: true"));
        assert!(!r.contains("location"));
        assert_eq!(signature_report(b"[]"), "No digital signatures found.");
    }

    #[test]
    fn scanner_split_and_url() {
        let (cfg, rx) = mock("200 OK", &[("Content-Disposition", "attachment; filename*=UTF-8''scan%20parts.zip")], b"PK");
        assert_eq!(scanner_image_split(&cfg, "scan.png", b"IMG").unwrap().ext, "zip");
        let body = rx.recv().unwrap().body_str();
        assert!(body.contains("filename=\"scan.png\"\r\nContent-Type: image/png\r\n"));
        assert!(body.contains("name=\"minArea\"\r\n\r\n8000\r\n"));
        let (cfg, rx) = mock("200 OK", &[("Content-Type", "application/pdf")], b"%PDF");
        url_to_pdf(&cfg, " https://example.org/page ").unwrap();
        let req = rx.recv().unwrap();
        assert!(req.head.starts_with("POST /api/v1/convert/url/pdf "));
        assert!(req.body_str().contains("name=\"urlInput\"\r\n\r\nhttps://example.org/page\r\n"));
        assert!(url_to_pdf(&cfg, "ftp://x").is_err());
    }

    #[test]
    fn reply_ext_from_headers() {
        assert_eq!(reply_ext(Some("attachment; filename=\"a.b.DOCX\""), Some("application/pdf")).as_deref(), Some("docx"));
        assert_eq!(reply_ext(Some("attachment; filename=out.zip"), None).as_deref(), Some("zip"));
        assert_eq!(reply_ext(Some("inline; filename*=UTF-8''r%C3%A9sum%C3%A9.odt"), None).as_deref(), Some("odt"));
        assert_eq!(reply_ext(Some("attachment"), Some("application/pdf;charset=UTF-8")).as_deref(), Some("pdf"));
        assert_eq!(reply_ext(None, Some("text/csv")).as_deref(), Some("csv"));
        assert_eq!(reply_ext(None, Some("application/x-unknown")), None);
        assert_eq!(reply_ext(None, None), None);
    }

    #[test]
    fn url_join() {
        assert_eq!(join("https://h:8081/", "/api/v1/x").unwrap(), "https://h:8081/api/v1/x");
        assert_eq!(join("h.ts.net:8081", "api/v1/x").unwrap(), "https://h.ts.net:8081/api/v1/x");
        assert_eq!(join("http://h/stirling", "/a").unwrap(), "http://h/stirling/a");
        assert!(join("  ", "/a").is_err());
        assert!(join("ftp://h", "/a").is_err());
    }
}
