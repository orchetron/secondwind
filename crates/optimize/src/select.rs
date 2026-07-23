use serde_json::Value;

use crate::atom::canonicalize;

// One slice of an offloaded block: a `/`-path of `field=value` / `[n]` / field segments, or `L<a>-<b>`.
// A string leaf returns verbatim; a structured slice returns canonical JSON.
pub fn select(block: &str, selector: &str) -> Option<String> {
    if let Some(range) = selector.strip_prefix('L') {
        return line_range(block, range);
    }
    let root: Value = serde_json::from_str(block).ok()?;
    let mut cur = &root;
    for seg in selector.split('/').filter(|s| !s.is_empty()) {
        cur = step(cur, seg)?;
    }
    Some(match cur {
        Value::String(s) => s.clone(),
        other => canonicalize(other),
    })
}

fn step<'a>(cur: &'a Value, seg: &str) -> Option<&'a Value> {
    if let Some(idx) = seg.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return cur.as_array()?.get(idx.parse::<usize>().ok()?);
    }
    if let Some((key, want)) = seg.split_once('=') {
        return cur
            .as_array()?
            .iter()
            .find(|el| el.get(key).and_then(scalar).as_deref() == Some(want));
    }
    cur.get(seg)
}

// A scalar rendered as the plain string a selector compares against (numbers/bools without quotes).
fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn line_range(block: &str, range: &str) -> Option<String> {
    let (a, b) = range.split_once('-')?;
    let (a, b) = (
        a.trim().parse::<usize>().ok()?,
        b.trim().parse::<usize>().ok()?,
    );
    if a == 0 || a > b {
        return None;
    }
    let lines: Vec<&str> = block.lines().collect();
    if a > lines.len() {
        return None;
    }
    Some(lines[a - 1..b.min(lines.len())].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issues() -> String {
        r#"[{"id":13932,"state":"open","body":"crash on Termux"},
            {"id":13939,"state":"closed","body":"symlink bug in gh skill install"}]"#
            .to_string()
    }

    #[test]
    fn finds_a_record_by_key_and_returns_its_values() {
        let out = select(&issues(), "id=13932").unwrap();
        assert!(out.contains("crash on Termux"));
        assert!(!out.contains("symlink bug"), "only the matched record");
    }

    #[test]
    fn drills_into_one_field_verbatim() {
        assert_eq!(
            select(&issues(), "id=13939/body").unwrap(),
            "symlink bug in gh skill install"
        );
    }

    #[test]
    fn indexes_an_array() {
        assert!(select(&issues(), "[0]/body").unwrap().contains("Termux"));
    }

    #[test]
    fn nested_object_path() {
        let block = r#"{"user":{"login":"octocat","id":1}}"#;
        assert_eq!(select(block, "user/login").unwrap(), "octocat");
    }

    #[test]
    fn line_range_on_text() {
        let text = "a\nb\nc\nd\ne";
        assert_eq!(select(text, "L2-4").unwrap(), "b\nc\nd");
    }

    #[test]
    fn a_miss_is_none_not_a_wrong_slice() {
        assert_eq!(select(&issues(), "id=99999"), None);
        assert_eq!(select(&issues(), "id=13932/nope"), None);
        assert_eq!(select("not json", "id=1"), None);
    }
}
