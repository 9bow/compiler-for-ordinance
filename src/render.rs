//! Renders parsed ordinance data into Markdown bytes and commit messages.
//!
//! Mirrors `legalize-pipeline/ordinances/converter.py::xml_to_markdown` and
//! `compute_path`.

use std::sync::OnceLock;

use anyhow::Result;
use regex::Regex;
use serde::Serialize;
use unicode_normalization::UnicodeNormalization;

use crate::git_repo::RepoPathBuf;
use crate::jurisdictions::{classify_type, split_jurisdiction};
use crate::xml_parser::{OrdinanceDetail, OrdinanceMetadata};

/// Pattern that strips `<br>` / `<br/>` tags.
fn br_re() -> &'static Regex {
    static INSTANCE: OnceLock<Regex> = OnceLock::new();
    INSTANCE.get_or_init(|| Regex::new(r"(?i)<br\s*/?>").unwrap())
}

/// Pattern that strips any remaining HTML tag.
fn html_tag_re() -> &'static Regex {
    static INSTANCE: OnceLock<Regex> = OnceLock::new();
    INSTANCE.get_or_init(|| Regex::new(r"<[^>]+>").unwrap())
}

/// Pattern that collapses three or more consecutive newlines.
fn multi_blank_re() -> &'static Regex {
    static INSTANCE: OnceLock<Regex> = OnceLock::new();
    INSTANCE.get_or_init(|| Regex::new(r"\n{3,}").unwrap())
}

/// Pattern that matches filesystem-unsafe characters during name sanitization.
fn path_unsafe_re() -> &'static Regex {
    static INSTANCE: OnceLock<Regex> = OnceLock::new();
    INSTANCE.get_or_init(|| Regex::new(r#"[\\/:*?"<>|]"#).unwrap())
}

/// Pattern that collapses whitespace runs in sanitized names.
fn whitespace_re() -> &'static Regex {
    static INSTANCE: OnceLock<Regex> = OnceLock::new();
    INSTANCE.get_or_init(|| Regex::new(r"\s+").unwrap())
}

/// Pattern that matches the inline `제N조(제목) ` prefix duplicated inside 조내용.
fn article_header_re() -> &'static Regex {
    static INSTANCE: OnceLock<Regex> = OnceLock::new();
    INSTANCE.get_or_init(|| Regex::new(r"^제\d+조\s*(?:\([^)]*\))?\s*").unwrap())
}

/// Pattern matching an HWP/HWPX extension used to flag attachments.
fn hwp_ext_re() -> &'static Regex {
    static INSTANCE: OnceLock<Regex> = OnceLock::new();
    INSTANCE.get_or_init(|| Regex::new(r"(?i)\.hwpx?$").unwrap())
}

/// NFC-normalizes, strips path-unsafe bytes, collapses whitespace.
pub fn sanitize_name(name: &str) -> String {
    let nfc: String = name.nfc().collect();
    let trimmed = nfc.trim();
    let replaced = path_unsafe_re().replace_all(trimmed, "_").into_owned();
    let collapsed = whitespace_re().replace_all(&replaced, " ").into_owned();
    collapsed.trim().to_owned()
}

/// Normalizes a raw 조문번호 like `"000100"` → `"1"`, `"000102"` → `"1의2"`.
pub fn format_article_number(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.len() >= 4 && trimmed.bytes().all(|b| b.is_ascii_digit()) {
        let split = trimmed.len() - 2;
        let main: u32 = trimmed[..split].parse().unwrap_or(0);
        let sub: u32 = trimmed[split..].parse().unwrap_or(0);
        if sub == 0 {
            return main.to_string();
        }
        return format!("{main}의{sub}");
    }
    trimmed.to_owned()
}

/// Strips HTML tags and collapses whitespace to plain Markdown.
pub fn html_to_markdown(input: &str) -> String {
    let with_newlines = br_re().replace_all(input, "\n").into_owned();
    let stripped = html_tag_re().replace_all(&with_newlines, "").into_owned();
    let decoded = stripped
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    let collapsed = multi_blank_re().replace_all(&decoded, "\n\n").into_owned();
    collapsed.trim().to_owned()
}

/// Converts a `YYYYMMDD` string to `YYYY-MM-DD`; returns the original trimmed value otherwise.
pub fn format_date(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() == 8 && trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return format!("{}-{}-{}", &trimmed[..4], &trimmed[4..6], &trimmed[6..8]);
    }
    trimmed.to_owned()
}

/// Computes the repository path for an ordinance.
///
/// Layout: `ordinances/{광역}/{기초|_본청}/{ordinance_type}/{NFC(자치법규명)}/본문.md`.
/// Returns `None` when either `ordinance_type` or the sanitized name is empty.
pub fn compute_path(metadata: &OrdinanceMetadata) -> Option<RepoPathBuf> {
    let ordinance_type = classify_type(&metadata.type_raw);
    if ordinance_type.is_empty() {
        return None;
    }
    let name = sanitize_name(&metadata.name);
    if name.is_empty() {
        return None;
    }

    let (gwang_raw, gicho_raw) = split_jurisdiction(&metadata.jurisdiction);
    let gwang_clean = sanitize_name(&gwang_raw);
    let gwang = if gwang_clean.is_empty() {
        String::from("_미상")
    } else {
        gwang_clean
    };
    let gicho = match gicho_raw {
        Some(g) => {
            let cleaned = sanitize_name(&g);
            if cleaned.is_empty() {
                String::from("_본청")
            } else {
                cleaned
            }
        }
        None => String::from("_본청"),
    };

    Some(RepoPathBuf::ordinance_file(
        gwang,
        gicho,
        ordinance_type,
        name,
    ))
}

/// Renders one parsed ordinance document into the repository Markdown format.
pub fn ordinance_to_markdown(detail: &OrdinanceDetail) -> Result<Vec<u8>> {
    let ordinance_type = classify_type(&detail.metadata.type_raw);
    let has_articles = !detail.body.articles.is_empty();
    let attachments = detail.body.attachments.clone();
    let attachments_hwp = attachments.iter().any(|a| hwp_ext_re().is_match(a));

    let body_format = if has_articles && attachments_hwp {
        "mixed"
    } else if has_articles {
        "text"
    } else {
        "hwp_only"
    };
    let body_source = if matches!(body_format, "text" | "mixed") {
        "api-text"
    } else {
        "parsing-failed"
    };

    let mut body = render_body(&detail.body);
    if !has_articles && body.is_empty() {
        body = if attachments_hwp {
            format!(
                "> 본문은 첨부파일(HWP)로 제공됩니다. 첨부파일 목록: {}",
                attachments.join(", ")
            )
        } else {
            String::from("> 본문이 제공되지 않습니다.")
        };
    }

    let frontmatter = Frontmatter {
        jurisdiction: &detail.metadata.jurisdiction,
        jurisdiction_code: &detail.metadata.jurisdiction_code,
        ordinance_type,
        ordinance_id: &detail.metadata.ordinance_id,
        name: &detail.metadata.name,
        promulgation_date: format_date(&detail.metadata.promulgation_date),
        promulgation_no: &detail.metadata.promulgation_no,
        enforcement_date: format_date(&detail.metadata.enforcement_date),
        revision_kind: &detail.metadata.revision_kind,
        department: &detail.metadata.department,
        related_laws: detail.body.related_laws.clone(),
        attachments,
        attachments_hwp,
        body_format,
        body_source,
        hwp_sha256: "",
    };
    let mut yaml = serde_yaml::to_string(&frontmatter)?;
    if let Some(stripped) = yaml.strip_prefix("---\n") {
        yaml = stripped.to_owned();
    }

    Ok(format!("---\n{yaml}---\n\n{body}\n").into_bytes())
}

/// Renders the ordinance body (articles + appendix) as plain Markdown.
fn render_body(body: &crate::xml_parser::OrdinanceBody) -> String {
    let mut parts: Vec<String> = Vec::new();
    for article in &body.articles {
        let number = format_article_number(&article.number_raw);
        if !number.is_empty() {
            let header = if article.title.is_empty() {
                format!("##### 제{number}조")
            } else {
                format!("##### 제{number}조 ({})", article.title)
            };
            parts.push(header);
        }
        if !article.content.is_empty() {
            let rendered = html_to_markdown(&article.content);
            let stripped = article_header_re().replace(&rendered, "").into_owned();
            let stripped = stripped.trim();
            if !stripped.is_empty() {
                parts.push(stripped.to_owned());
            }
        }
    }

    for appendix in &body.appendix {
        if appendix.trim().is_empty() {
            continue;
        }
        parts.push(String::from("## 부칙"));
        parts.push(html_to_markdown(appendix));
    }

    parts.join("\n\n").trim().to_owned()
}

/// Builds the Git commit message for one ordinance revision.
pub fn build_commit_message(metadata: &OrdinanceMetadata) -> String {
    let ordinance_type = classify_type(&metadata.type_raw);
    let header_type = if ordinance_type.is_empty() {
        "자치법규"
    } else {
        ordinance_type
    };
    let title = if !metadata.name.is_empty() {
        format!("{header_type}: {}", metadata.name)
    } else {
        format!("{header_type}: {}", metadata.ordinance_id)
    };
    let date_line = format_date(&metadata.promulgation_date);
    let mut lines: Vec<String> = Vec::new();
    lines.push(title);
    lines.push(String::new());
    lines.push(format!("공포일자: {date_line}"));
    if !metadata.promulgation_no.is_empty() {
        lines.push(format!("공포번호: {}", metadata.promulgation_no));
    }
    if !metadata.jurisdiction.is_empty() {
        lines.push(format!("지자체: {}", metadata.jurisdiction));
    }
    if !metadata.revision_kind.is_empty() {
        lines.push(format!("제개정: {}", metadata.revision_kind));
    }
    lines.push(format!("자치법규ID: {}", metadata.ordinance_id));
    lines.join("\n")
}

/// YAML frontmatter payload. Key order matches `converter.py::xml_to_markdown`.
#[derive(Debug, Serialize)]
struct Frontmatter<'a> {
    /// 지자체기관명 (raw, jurisdictions.py splits for path only).
    jurisdiction: &'a str,
    /// 지자체기관코드.
    jurisdiction_code: &'a str,
    /// Normalized 자치법규종류 (조례/규칙/훈령/예규).
    ordinance_type: &'a str,
    /// 자치법규ID.
    #[serde(rename = "자치법규ID")]
    ordinance_id: &'a str,
    /// 자치법규명.
    #[serde(rename = "자치법규명")]
    name: &'a str,
    /// 공포일자 (`YYYY-MM-DD` when possible).
    #[serde(rename = "공포일자")]
    promulgation_date: String,
    /// 공포번호.
    #[serde(rename = "공포번호")]
    promulgation_no: &'a str,
    /// 시행일자 (`YYYY-MM-DD` when possible).
    #[serde(rename = "시행일자")]
    enforcement_date: String,
    /// 제개정구분.
    #[serde(rename = "제개정구분")]
    revision_kind: &'a str,
    /// 담당부서명.
    #[serde(rename = "담당부서명")]
    department: &'a str,
    /// List of referenced law names.
    related_laws: Vec<String>,
    /// List of attachment filenames.
    attachments: Vec<String>,
    /// Whether at least one attachment is an HWP/HWPX file.
    attachments_hwp: bool,
    /// `"text"`, `"mixed"`, or `"hwp_only"`.
    body_format: &'static str,
    /// `"api-text"` or `"parsing-failed"`.
    body_source: &'static str,
    /// Placeholder for a future HWP body SHA-256; empty for now.
    hwp_sha256: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml_parser::{Article, OrdinanceBody};

    fn sample_detail() -> OrdinanceDetail {
        OrdinanceDetail {
            metadata: OrdinanceMetadata {
                ordinance_id: String::from("2000111"),
                name: String::from("서울특별시 간행물에 의한 광고계약 및 해지 등에 관한 조례"),
                type_raw: String::from("C0001"),
                jurisdiction: String::from("서울특별시"),
                jurisdiction_code: String::new(),
                promulgation_date: String::from("20210930"),
                promulgation_no: String::from("8127"),
                enforcement_date: String::from("20220113"),
                revision_kind: String::from("타법개정"),
                department: String::from("홍보담당관"),
            },
            body: OrdinanceBody {
                articles: vec![Article {
                    number_raw: String::from("000100"),
                    title: String::from("목적"),
                    content: String::from("제1조(목적) 이 조례는 목적을 규정한다."),
                }],
                appendix: vec![String::from("이 조례는 공포한 날부터 시행한다.")],
                attachments: vec![],
                related_laws: vec![],
            },
        }
    }

    #[test]
    fn formats_article_numbers() {
        assert_eq!(format_article_number("000100"), "1");
        assert_eq!(format_article_number("000102"), "1의2");
        assert_eq!(format_article_number(""), "");
        assert_eq!(format_article_number("1의2"), "1의2");
    }

    #[test]
    fn formats_dates() {
        assert_eq!(format_date("20210930"), "2021-09-30");
        assert_eq!(format_date(""), "");
        assert_eq!(format_date("2021.9.30"), "2021.9.30");
    }

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize_name(" 서울 / 조례 "), "서울 _ 조례");
        assert_eq!(sanitize_name("강남구"), "강남구");
    }

    #[test]
    fn computes_path_for_gwangyeok_only() {
        let detail = sample_detail();
        let path = compute_path(&detail.metadata).unwrap();
        assert_eq!(
            path.to_string(),
            "ordinances/서울특별시/_본청/조례/서울특별시 간행물에 의한 광고계약 및 해지 등에 관한 조례/본문.md"
        );
    }

    #[test]
    fn computes_path_with_gicho() {
        let mut detail = sample_detail();
        detail.metadata.jurisdiction = String::from("서울특별시 강남구");
        detail.metadata.name = String::from("강남구 어떤 조례");
        let path = compute_path(&detail.metadata).unwrap();
        assert_eq!(
            path.to_string(),
            "ordinances/서울특별시/강남구/조례/강남구 어떤 조례/본문.md"
        );
    }

    #[test]
    fn renders_markdown_document() {
        let detail = sample_detail();
        let rendered = ordinance_to_markdown(&detail).unwrap();
        let text = String::from_utf8(rendered).unwrap();
        assert!(text.starts_with("---\n"));
        assert!(text.contains("ordinance_type: 조례"));
        assert!(text.contains("자치법규ID: '2000111'"));
        assert!(text.contains("공포일자: 2021-09-30"));
        assert!(text.contains("##### 제1조 (목적)"));
        assert!(text.contains("이 조례는 목적을 규정한다."));
        assert!(text.contains("## 부칙"));
    }

    #[test]
    fn builds_commit_message() {
        let detail = sample_detail();
        let msg = build_commit_message(&detail.metadata);
        assert!(msg.starts_with("조례: 서울특별시 간행물에"));
        assert!(msg.contains("공포일자: 2021-09-30"));
        assert!(msg.contains("자치법규ID: 2000111"));
    }
}
