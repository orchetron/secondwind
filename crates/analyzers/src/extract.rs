const MIN_TRACKED_DIGITS: usize = 2;
const MAX_ITEMS_PER_SEGMENT: usize = 200;
const MAX_ITEMS_UNCAPPED: usize = usize::MAX;

pub fn numbers_all(text: &str) -> Vec<String> {
    numbers_capped(text, MAX_ITEMS_UNCAPPED)
}

pub fn artifacts_all(text: &str) -> Vec<String> {
    artifacts_capped(text, MAX_ITEMS_UNCAPPED)
}

pub fn keyed_numbers(text: &str) -> Vec<(String, String)> {
    let bytes = text.as_bytes();
    let mut pairs = Vec::new();
    let mut i = 0;
    while i < bytes.len() && pairs.len() < MAX_ITEMS_PER_SEGMENT {
        if !is_ident_start(bytes[i]) {
            i += 1;
            continue;
        }
        if i > 0 && bytes[i - 1] == b'.' {
            while i < bytes.len() && is_ident_char(bytes[i]) {
                i += 1;
            }
            continue;
        }
        let key_start = i;
        while i < bytes.len() && is_ident_char(bytes[i]) {
            i += 1;
        }
        let key_end = i;
        let mut j = i;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'"' || bytes[j] == b'\'') {
            j += 1;
        }
        if j >= bytes.len() || (bytes[j] != b'=' && bytes[j] != b':') {
            continue;
        }
        j += 1;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'"' || bytes[j] == b'\'') {
            j += 1;
        }
        let value_start = j;
        let mut seen_dot = false;
        while j < bytes.len() && (bytes[j].is_ascii_digit() || (bytes[j] == b'.' && !seen_dot)) {
            if bytes[j] == b'.' {
                seen_dot = true;
            }
            j += 1;
        }
        if j > value_start && bytes[value_start].is_ascii_digit() {
            let value = &text[value_start..j];
            if !value.ends_with('.') {
                pairs.push((text[key_start..key_end].to_string(), value.to_string()));
                i = j;
            }
        }
    }
    pairs
}

pub fn numbers(text: &str) -> Vec<String> {
    numbers_capped(text, MAX_ITEMS_PER_SEGMENT)
}

fn numbers_capped(text: &str, cap: usize) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() && found.len() < cap {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let preceded_by_word = i > 0 && (is_ident_start(bytes[i - 1]) || bytes[i - 1] == b'.');
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let followed_by_word = i < bytes.len() && is_ident_start(bytes[i]);
        if preceded_by_word || followed_by_word {
            continue;
        }
        let value = &text[start..i];
        if value.len() >= MIN_TRACKED_DIGITS && !found.iter().any(|v| v == value) {
            found.push(value.to_string());
        }
    }
    found
}

pub fn artifacts(text: &str) -> Vec<String> {
    artifacts_capped(text, MAX_ITEMS_PER_SEGMENT)
}

fn artifacts_capped(text: &str, cap: usize) -> Vec<String> {
    let mut found = Vec::new();
    for raw in text.split_whitespace() {
        if found.len() >= cap {
            break;
        }
        let token = raw.trim_matches(|c: char| "\"'(),;[]{}<>`".contains(c));
        if token.len() < 4 {
            continue;
        }
        let candidate = if token.contains('/') {
            let path = token.split(':').next().unwrap_or(token);
            if path.contains('/') && path.chars().any(|c| c.is_ascii_alphanumeric()) {
                Some(path)
            } else {
                None
            }
        } else if token.starts_with("toolu_")
            || token.starts_with("req_")
            || (token.len() >= 12 && token.chars().all(|c| c.is_ascii_hexdigit()))
        {
            Some(token)
        } else {
            None
        };
        if let Some(c) = candidate
            && !found.iter().any(|f| f == c)
        {
            found.push(c.to_string());
        }
    }
    found
}

pub fn grep_claims(text: &str) -> Vec<(String, String)> {
    let mut claims = Vec::new();
    for line in text.lines() {
        if claims.len() >= MAX_ITEMS_PER_SEGMENT {
            break;
        }
        let mut parts = line.splitn(3, ':');
        let (Some(path), Some(line_no), Some(content)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if path.is_empty()
            || path.contains(' ')
            || path.contains('\t')
            || !(path.contains('/') || path.contains('.'))
            || line_no.is_empty()
            || !line_no.bytes().all(|b| b.is_ascii_digit())
        {
            continue;
        }
        claims.push((format!("{path}:{line_no}"), content.trim().to_string()));
    }
    claims
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyed_numbers_handles_common_shapes() {
        let pairs = keyed_numbers("retry_limit = 3, \"port\": 8443, timeout=30.5");
        assert!(pairs.contains(&("retry_limit".into(), "3".into())));
        assert!(pairs.contains(&("port".into(), "8443".into())));
        assert!(pairs.contains(&("timeout".into(), "30.5".into())));
    }

    #[test]
    fn numbers_skips_identifier_fragments() {
        let ns = numbers("mod_42 has 8443 items and abc0001 tag, plus 7");
        assert_eq!(ns, vec!["8443".to_string()]);
    }

    #[test]
    fn artifacts_finds_paths_and_ids() {
        let a = artifacts("read src/main.rs:42 then toolu_01AB and deadbeefcafe1234 done");
        assert!(a.contains(&"src/main.rs".to_string()));
        assert!(a.contains(&"toolu_01AB".to_string()));
        assert!(a.contains(&"deadbeefcafe1234".to_string()));
    }

    #[test]
    fn grep_claims_parses_path_line_content() {
        let c = grep_claims("src/app.rs:88:retry_limit = 3\nnot a claim line");
        assert_eq!(
            c,
            vec![("src/app.rs:88".to_string(), "retry_limit = 3".to_string())]
        );
    }
}
