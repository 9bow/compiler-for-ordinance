//! Jurisdiction-name parsing and ordinance-type classification.
//!
//! Mirrors `legalize-pipeline/ordinances/jurisdictions.py` and
//! `_classify_type` inside `legalize-pipeline/ordinances/converter.py`.

use std::sync::OnceLock;

use regex::Regex;

/// Seventeen 광역시·도 display names used to split `"서울특별시 송파구"` into
/// `(광역, 기초)`. Keep in sync with `jurisdictions.py::GWANGYEOK`.
pub const GWANGYEOK_NAMES: &[&str] = &[
    "서울특별시",
    "부산광역시",
    "대구광역시",
    "인천광역시",
    "광주광역시",
    "대전광역시",
    "울산광역시",
    "세종특별자치시",
    "경기도",
    "강원특별자치도",
    "충청북도",
    "충청남도",
    "전북특별자치도",
    "전라남도",
    "경상북도",
    "경상남도",
    "제주특별자치도",
];

/// Regex that identifies 교육청류 jurisdictions routed under `_본청`.
fn edu_office_re() -> &'static Regex {
    static INSTANCE: OnceLock<Regex> = OnceLock::new();
    INSTANCE.get_or_init(|| Regex::new(r"교육(청|청교육지원청|지원청)$").unwrap())
}

/// Splits a raw 지자체기관명 into `(광역, 기초|None)`.
///
/// Examples mirror the Python reference:
/// - `"서울특별시"` → `("서울특별시", None)`
/// - `"서울특별시 강남구"` → `("서울특별시", Some("강남구"))`
/// - `"서울특별시교육청"` → `("서울특별시교육청", None)` (교육청 sits at top-level)
pub fn split_jurisdiction(raw_name: &str) -> (String, Option<String>) {
    let raw = raw_name.trim();
    if raw.is_empty() {
        return (raw.to_owned(), None);
    }

    if edu_office_re().is_match(raw) {
        return (raw.to_owned(), None);
    }

    for gwang in GWANGYEOK_NAMES {
        if raw == *gwang {
            return ((*gwang).to_owned(), None);
        }
        let with_space = format!("{gwang} ");
        if let Some(rest) = raw.strip_prefix(&with_space) {
            let rest = rest.trim();
            if rest.is_empty() {
                return ((*gwang).to_owned(), None);
            }
            return ((*gwang).to_owned(), Some(rest.to_owned()));
        }
    }

    (raw.to_owned(), None)
}

/// Normalizes a raw 자치법규종류 field to one of `조례 / 규칙 / 훈령 / 예규`.
///
/// The upstream `자치법규종류` field may be a C000N code or a free-form label,
/// so this accepts both. Returns an empty string when nothing matches to stay
/// bit-compatible with the Python converter's "safer default".
pub fn classify_type(raw: &str) -> &'static str {
    let raw = raw.trim();
    if raw.is_empty() {
        return "";
    }
    match raw {
        "C0001" => return "조례",
        "C0002" => return "규칙",
        "C0003" => return "훈령",
        "C0004" => return "예규",
        _ => {}
    }
    const TYPES: [&str; 4] = ["조례", "규칙", "훈령", "예규"];
    for t in TYPES {
        if raw.contains(t) {
            return t;
        }
    }
    ""
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gwangyeok_only_has_no_gicho() {
        assert_eq!(
            split_jurisdiction("서울특별시"),
            ("서울특별시".to_owned(), None)
        );
        assert_eq!(
            split_jurisdiction("세종특별자치시"),
            ("세종특별자치시".to_owned(), None)
        );
    }

    #[test]
    fn gwangyeok_with_gicho_splits() {
        assert_eq!(
            split_jurisdiction("서울특별시 강남구"),
            ("서울특별시".to_owned(), Some("강남구".to_owned()))
        );
        assert_eq!(
            split_jurisdiction("경기도 성남시"),
            ("경기도".to_owned(), Some("성남시".to_owned()))
        );
    }

    #[test]
    fn edu_offices_stay_top_level() {
        assert_eq!(
            split_jurisdiction("서울특별시교육청"),
            ("서울특별시교육청".to_owned(), None)
        );
    }

    #[test]
    fn unknown_gwangyeok_passes_through() {
        assert_eq!(
            split_jurisdiction("미확인 어딘가"),
            ("미확인 어딘가".to_owned(), None)
        );
    }

    #[test]
    fn classify_type_maps_codes_and_labels() {
        assert_eq!(classify_type("C0001"), "조례");
        assert_eq!(classify_type("C0002"), "규칙");
        assert_eq!(classify_type("C0003"), "훈령");
        assert_eq!(classify_type("C0004"), "예규");
        assert_eq!(classify_type("조례"), "조례");
        assert_eq!(classify_type("서울특별시 조례"), "조례");
        assert_eq!(classify_type(""), "");
        assert_eq!(classify_type("알수없음"), "");
    }
}
