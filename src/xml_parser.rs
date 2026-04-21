//! XML parsing helpers for cached ELIS ordinance (`LawService`) documents.
//!
//! Mirrors `legalize-pipeline/ordinances/converter.py`.

use anyhow::{Context, Result};
use quick_xml::Reader;
use quick_xml::escape::unescape;
use quick_xml::events::Event;

/// Ordinance metadata extracted from a cached `LawService` document.
#[derive(Debug, Clone, Default)]
pub struct OrdinanceMetadata {
    /// 자치법규ID (also used as the cache filename stem).
    pub ordinance_id: String,
    /// 자치법규명.
    pub name: String,
    /// Raw 자치법규종류 (either a `C000N` code or a label).
    pub type_raw: String,
    /// 지자체기관명 (raw, normalization deferred to the path layer).
    pub jurisdiction: String,
    /// 지자체기관코드.
    pub jurisdiction_code: String,
    /// 공포일자 in `YYYYMMDD` form (used for commit date and frontmatter).
    pub promulgation_date: String,
    /// 공포번호.
    pub promulgation_no: String,
    /// 시행일자 in `YYYYMMDD` form.
    pub enforcement_date: String,
    /// 제개정정보 / 제개정구분(명).
    pub revision_kind: String,
    /// 담당부서명.
    pub department: String,
}

/// Fully parsed body including articles and appendix text, ready to render.
#[derive(Debug, Clone, Default)]
pub struct OrdinanceBody {
    /// Parsed articles (조).
    pub articles: Vec<Article>,
    /// 부칙 appendix blocks (raw `부칙내용`).
    pub appendix: Vec<String>,
    /// Attachment filenames collected in insertion order.
    pub attachments: Vec<String>,
    /// Referenced law names from 관련법령.
    pub related_laws: Vec<String>,
}

/// One 조 inside the ordinance body.
#[derive(Debug, Clone, Default)]
pub struct Article {
    /// Raw 조문번호 (e.g. `"000100"`, `"000102"`).
    pub number_raw: String,
    /// 조제목 / 조문제목 inner text.
    pub title: String,
    /// 조내용 / 조문내용 inner text (HTML-bearing).
    pub content: String,
}

/// Fully parsed ordinance document bundle.
#[derive(Debug, Clone, Default)]
pub struct OrdinanceDetail {
    /// Top-level metadata used for path planning and frontmatter.
    pub metadata: OrdinanceMetadata,
    /// Body sections and appendix blocks.
    pub body: OrdinanceBody,
}

/// Parses just the metadata needed for pass-1 ordering and path planning.
///
/// Returns `Ok(None)` when the root element is not `LawService` (for instance,
/// upstream HTML error pages) or when the ordinance id is missing.
pub fn parse_metadata_only(xml: &[u8]) -> Result<Option<OrdinanceMetadata>> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);

    let mut buf = Vec::new();
    let mut capture_tag: Option<String> = None;
    let mut capture_text = String::new();
    let mut metadata = OrdinanceMetadata::default();
    let mut root_seen = false;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(event) => {
                let tag = decode_name(event.name().as_ref())?;
                if !root_seen {
                    if tag != "LawService" {
                        return Ok(None);
                    }
                    root_seen = true;
                    buf.clear();
                    continue;
                }

                //
                // Mirror the Python `_text(root, ".//자치법규ID") or ...` chain by only capturing
                // the first non-empty hit, leaving later duplicates untouched.
                //
                let should_capture = match tag.as_str() {
                    "자치법규ID" | "자치법규일련번호" | "ID" => {
                        metadata.ordinance_id.is_empty()
                    }
                    "자치법규명" => metadata.name.is_empty(),
                    "자치법규종류" => metadata.type_raw.is_empty(),
                    "지자체기관명" => metadata.jurisdiction.is_empty(),
                    "지자체기관코드" | "기관코드" => {
                        metadata.jurisdiction_code.is_empty()
                    }
                    "공포일자" => metadata.promulgation_date.is_empty(),
                    "공포번호" => metadata.promulgation_no.is_empty(),
                    "시행일자" => metadata.enforcement_date.is_empty(),
                    "제개정정보" | "제개정구분명" | "제개정구분" => {
                        metadata.revision_kind.is_empty()
                    }
                    "담당부서명" => metadata.department.is_empty(),
                    _ => false,
                };
                if should_capture {
                    capture_text.clear();
                    capture_tag = Some(tag);
                }
            }
            Event::Empty(event) => {
                let tag = decode_name(event.name().as_ref())?;
                if !root_seen {
                    if tag != "LawService" {
                        return Ok(None);
                    }
                    return Ok(Some(metadata));
                }
            }
            Event::Text(text) => {
                if capture_tag.is_some() {
                    capture_text.push_str(&decode_text(text.as_ref())?);
                }
            }
            Event::CData(text) => {
                if capture_tag.is_some() {
                    capture_text.push_str(&String::from_utf8_lossy(text.as_ref()));
                }
            }
            Event::End(event) => {
                let tag = decode_name(event.name().as_ref())?;
                if let Some(current) = &capture_tag
                    && current == &tag
                {
                    let trimmed = capture_text.trim().to_owned();
                    match current.as_str() {
                        "자치법규ID" | "자치법규일련번호" | "ID" => {
                            if metadata.ordinance_id.is_empty() {
                                metadata.ordinance_id = trimmed;
                            }
                        }
                        "자치법규명" => metadata.name = trimmed,
                        "자치법규종류" => metadata.type_raw = trimmed,
                        "지자체기관명" => metadata.jurisdiction = trimmed,
                        "지자체기관코드" | "기관코드" => {
                            if metadata.jurisdiction_code.is_empty() {
                                metadata.jurisdiction_code = trimmed;
                            }
                        }
                        "공포일자" => metadata.promulgation_date = trimmed,
                        "공포번호" => metadata.promulgation_no = trimmed,
                        "시행일자" => metadata.enforcement_date = trimmed,
                        "제개정정보" | "제개정구분명" | "제개정구분" => {
                            if metadata.revision_kind.is_empty() {
                                metadata.revision_kind = trimmed;
                            }
                        }
                        "담당부서명" => metadata.department = trimmed,
                        _ => {}
                    }
                    capture_tag = None;
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    if !root_seen {
        return Ok(None);
    }
    Ok(Some(metadata))
}

/// Parses the full body needed for Markdown rendering.
pub fn parse_ordinance_body(xml: &[u8]) -> Result<OrdinanceBody> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);

    let mut buf = Vec::new();
    let mut body = OrdinanceBody::default();

    //
    // State machine: `path` tracks the element stack so we can scope which
    // captures apply inside `<조문>`, `<부칙>`, and attachment/related-law sections.
    //
    let mut path: Vec<String> = Vec::new();

    // Current article being assembled inside `<조문>/<조>` or `<조문단위>`.
    let mut current_article: Option<Article> = None;
    // Whether we are inside a `<조문>/<조>` container (the primary layout).
    let mut have_jo_in_jomun = false;

    // Active per-field capture inside the current article.
    let mut capture_field: Option<ArticleField> = None;
    // Active appendix/attachment/related text capture.
    let mut capture_generic: Option<GenericCapture> = None;
    // Current 부칙 block pending append into `body.appendix`.
    let mut buchik_inner: Option<String> = None;

    let mut text_buf = String::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(event) => {
                let tag = decode_name(event.name().as_ref())?;
                path.push(tag.clone());

                // Open an article container.
                if tag == "조" && path_contains(&path, "조문") {
                    current_article = Some(Article::default());
                    have_jo_in_jomun = true;
                } else if tag == "조문단위" && !have_jo_in_jomun {
                    // Legacy fallback only used when no `<조문>/<조>` container exists.
                    current_article = Some(Article::default());
                }

                // Open a 부칙 block.
                if tag == "부칙" {
                    buchik_inner = Some(String::new());
                }

                // Per-article field captures.
                if current_article.is_some() {
                    capture_field = match tag.as_str() {
                        "조문번호" => Some(ArticleField::Number),
                        "조제목" | "조문제목" => Some(ArticleField::Title),
                        "조내용" | "조문내용" => Some(ArticleField::Content),
                        _ => capture_field,
                    };
                }

                // Generic captures scoped by ancestry.
                capture_generic = match tag.as_str() {
                    "부칙내용" if path_contains(&path, "부칙") => {
                        Some(GenericCapture::Buchik)
                    }
                    "첨부파일명" | "별표서식파일명" | "별표파일명" | "파일명" => {
                        Some(GenericCapture::Attachment)
                    }
                    "관련법령명" | "관련법령" | "상위법령명" => {
                        Some(GenericCapture::Related)
                    }
                    _ => capture_generic,
                };

                if capture_field.is_some() || capture_generic.is_some() {
                    text_buf.clear();
                }
            }
            Event::Empty(_) => {
                // No-op: empty elements never carry text we care about here.
            }
            Event::Text(text) => {
                if capture_field.is_some() || capture_generic.is_some() {
                    text_buf.push_str(&decode_text(text.as_ref())?);
                }
            }
            Event::CData(text) => {
                if capture_field.is_some() || capture_generic.is_some() {
                    text_buf.push_str(&String::from_utf8_lossy(text.as_ref()));
                }
            }
            Event::End(event) => {
                let tag = decode_name(event.name().as_ref())?;

                // Finalize per-article field capture on matching end tag.
                if let (Some(field), Some(article)) =
                    (capture_field.as_ref(), current_article.as_mut())
                {
                    let matches = match field {
                        ArticleField::Number => tag == "조문번호",
                        ArticleField::Title => tag == "조제목" || tag == "조문제목",
                        ArticleField::Content => tag == "조내용" || tag == "조문내용",
                    };
                    if matches {
                        let captured = text_buf.trim().to_owned();
                        match field {
                            ArticleField::Number => {
                                if article.number_raw.is_empty() {
                                    article.number_raw = captured;
                                }
                            }
                            ArticleField::Title => {
                                if article.title.is_empty() {
                                    article.title = captured;
                                }
                            }
                            ArticleField::Content => {
                                if article.content.is_empty() {
                                    article.content = captured;
                                }
                            }
                        }
                        capture_field = None;
                        text_buf.clear();
                    }
                }

                // Finalize generic capture.
                if let Some(capture) = capture_generic.as_ref() {
                    let matches = match capture {
                        GenericCapture::Buchik => tag == "부칙내용",
                        GenericCapture::Attachment => matches!(
                            tag.as_str(),
                            "첨부파일명" | "별표서식파일명" | "별표파일명" | "파일명"
                        ),
                        GenericCapture::Related => {
                            matches!(tag.as_str(), "관련법령명" | "관련법령" | "상위법령명")
                        }
                    };
                    if matches {
                        let captured = text_buf.trim().to_owned();
                        match capture {
                            GenericCapture::Buchik => {
                                if let Some(slot) = buchik_inner.as_mut()
                                    && slot.is_empty()
                                    && !captured.is_empty()
                                {
                                    *slot = captured;
                                }
                            }
                            GenericCapture::Attachment => {
                                if !captured.is_empty()
                                    && !body.attachments.iter().any(|a| a == &captured)
                                {
                                    body.attachments.push(captured);
                                }
                            }
                            GenericCapture::Related => {
                                if !captured.is_empty()
                                    && !body.related_laws.iter().any(|a| a == &captured)
                                {
                                    body.related_laws.push(captured);
                                }
                            }
                        }
                        capture_generic = None;
                        text_buf.clear();
                    }
                }

                // Close article container.
                if (tag == "조" || tag == "조문단위")
                    && let Some(article) = current_article.take()
                {
                    let has_data = !article.number_raw.is_empty()
                        || !article.title.is_empty()
                        || !article.content.is_empty();
                    if has_data {
                        body.articles.push(article);
                    }
                }

                // Close 부칙 container.
                if tag == "부칙"
                    && let Some(inner) = buchik_inner.take()
                    && !inner.is_empty()
                {
                    body.appendix.push(inner);
                }

                path.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(body)
}

/// Which per-article field is currently being captured.
#[derive(Debug, Clone, Copy)]
enum ArticleField {
    /// 조문번호.
    Number,
    /// 조제목 / 조문제목.
    Title,
    /// 조내용 / 조문내용.
    Content,
}

/// Which generic (non-article) field is currently being captured.
#[derive(Debug, Clone, Copy)]
enum GenericCapture {
    /// 부칙내용 inside a 부칙 container.
    Buchik,
    /// An attachment filename under 첨부파일명 / 별표서식파일명 / 별표파일명 / 파일명.
    Attachment,
    /// A referenced law name under 관련법령명 / 관련법령 / 상위법령명.
    Related,
}

/// Returns `true` when `ancestor` appears anywhere in `path`.
fn path_contains(path: &[String], ancestor: &str) -> bool {
    path.iter().any(|segment| segment == ancestor)
}

/// Decodes one XML element name from UTF-8 bytes.
fn decode_name(name: &[u8]) -> Result<String> {
    Ok(std::str::from_utf8(name)
        .context("element name is not valid UTF-8")?
        .to_owned())
}

/// Decodes and unescapes one XML text node.
fn decode_text(text: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(text).context("text node is not valid UTF-8")?;
    Ok(unescape(text)?.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<LawService>
  <자치법규기본정보>
    <자치법규ID>2000111</자치법규ID>
    <공포일자>20210930</공포일자>
    <공포번호>8127</공포번호>
    <자치법규명><![CDATA[서울특별시 간행물에 의한 광고계약 및 해지 등에 관한 조례]]></자치법규명>
    <시행일자>20220113</시행일자>
    <자치법규종류>C0001</자치법규종류>
    <지자체기관명>서울특별시</지자체기관명>
    <담당부서명>홍보담당관</담당부서명>
    <제개정정보>타법개정</제개정정보>
  </자치법규기본정보>
  <조문>
    <조 조문번호='000100'>
      <조문번호>000100</조문번호>
      <조제목><![CDATA[목적]]></조제목>
      <조내용><![CDATA[제1조(목적) 이 조례는 목적을 규정한다.]]></조내용>
    </조>
  </조문>
  <부칙>
    <부칙내용><![CDATA[이 조례는 공포한 날부터 시행한다.]]></부칙내용>
  </부칙>
</LawService>"#;

    #[test]
    fn parses_metadata_for_law_service_xml() {
        let metadata = parse_metadata_only(SAMPLE_XML.as_bytes()).unwrap().unwrap();
        assert_eq!(metadata.ordinance_id, "2000111");
        assert_eq!(
            metadata.name,
            "서울특별시 간행물에 의한 광고계약 및 해지 등에 관한 조례"
        );
        assert_eq!(metadata.type_raw, "C0001");
        assert_eq!(metadata.jurisdiction, "서울특별시");
        assert_eq!(metadata.promulgation_date, "20210930");
        assert_eq!(metadata.promulgation_no, "8127");
        assert_eq!(metadata.enforcement_date, "20220113");
        assert_eq!(metadata.revision_kind, "타법개정");
        assert_eq!(metadata.department, "홍보담당관");
    }

    #[test]
    fn returns_none_for_non_law_service_root() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?><Other/>"#;
        assert!(parse_metadata_only(xml.as_bytes()).unwrap().is_none());
    }

    #[test]
    fn parses_body_articles_and_appendix() {
        let body = parse_ordinance_body(SAMPLE_XML.as_bytes()).unwrap();
        assert_eq!(body.articles.len(), 1);
        assert_eq!(body.articles[0].number_raw, "000100");
        assert_eq!(body.articles[0].title, "목적");
        assert!(body.articles[0].content.contains("제1조"));
        assert_eq!(body.appendix.len(), 1);
        assert!(body.appendix[0].contains("공포한 날"));
    }
}
