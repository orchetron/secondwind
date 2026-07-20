const HEADER: &str = "SWLOG";
const SLOT: char = '\u{0}';

pub struct Templated {
    pub wire: String,
    pub decoded: String,
}

// Lossless line templating for logs: each line's digit runs are masked to a slot and captured exactly,
// so lines sharing a template store it once. Aborts if the block already contains the slot byte.
pub fn try_template(raw: &str) -> Option<Templated> {
    if raw.contains(SLOT) || raw.lines().count() < 2 {
        return None;
    }

    let mut templates: Vec<String> = Vec::new();
    let mut index_of: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut rows: Vec<(usize, Vec<String>)> = Vec::new();

    for line in raw.split('\n') {
        let (template, vars) = mask(line);
        let idx = *index_of.entry(template.clone()).or_insert_with(|| {
            templates.push(template);
            templates.len() - 1
        });
        rows.push((idx, vars));
    }

    let mut wire = format!("{HEADER} {}\n{}", rows.len(), templates.len());
    for template in &templates {
        wire.push('\n');
        wire.push_str(&string_token(template));
    }
    for (idx, vars) in &rows {
        wire.push('\n');
        wire.push_str(&idx.to_string());
        for var in vars {
            wire.push('\t');
            wire.push_str(var);
        }
    }

    let decoded = decode(&wire)?;
    if decoded != raw {
        return None;
    }
    Some(Templated { wire, decoded })
}

pub fn decode(wire: &str) -> Option<String> {
    let mut lines = wire.split('\n');
    let count: usize = lines.next()?.strip_prefix(HEADER)?.trim().parse().ok()?;
    let n_templates: usize = lines.next()?.parse().ok()?;

    let mut templates = Vec::with_capacity(n_templates);
    for _ in 0..n_templates {
        let template: String = serde_json::from_str(lines.next()?).ok()?;
        templates.push(template);
    }

    let mut out: Vec<String> = Vec::with_capacity(count);
    for _ in 0..count {
        let row = lines.next()?;
        let mut fields = row.split('\t');
        let idx: usize = fields.next()?.parse().ok()?;
        let vars: Vec<&str> = fields.collect();
        out.push(unmask(templates.get(idx)?, &vars)?);
    }
    if lines.next().is_some() {
        return None;
    }
    Some(out.join("\n"))
}

fn mask(line: &str) -> (String, Vec<String>) {
    let mut template = String::with_capacity(line.len());
    let mut vars = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            let mut run = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() {
                    run.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            template.push(SLOT);
            vars.push(run);
        } else {
            template.push(c);
            chars.next();
        }
    }
    (template, vars)
}

fn unmask(template: &str, vars: &[&str]) -> Option<String> {
    let mut out = String::with_capacity(template.len());
    let mut next = 0;
    for c in template.chars() {
        if c == SLOT {
            out.push_str(vars.get(next)?);
            next += 1;
        } else {
            out.push(c);
        }
    }
    if next == vars.len() { Some(out) } else { None }
}

fn string_token(s: &str) -> String {
    serde_json::to_string(s).expect("string serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factors_shared_templates_and_round_trips() {
        let lines: Vec<String> = (0..200)
            .map(|i| {
                format!(
                    "2026-07-16 worker {i} finished job {} in {}ms",
                    i * 3,
                    i % 50
                )
            })
            .collect();
        let raw = lines.join("\n");
        let out = try_template(&raw).unwrap();
        assert!(out.wire.len() < raw.len());
        assert_eq!(decode(&out.wire).unwrap(), raw);
    }

    #[test]
    fn round_trips_a_small_block_even_without_saving() {
        let raw = "worker 1 ok in 12ms\nworker 2 ok in 8ms";
        let out = try_template(raw).unwrap();
        assert_eq!(decode(&out.wire).unwrap(), raw);
    }

    #[test]
    fn preserves_exact_numbers_and_paths() {
        let raw = "GET /srv/app_29/cfg.yaml 200 in 5ms\nGET /srv/app_58/cfg.yaml 500 in 9ms";
        let out = try_template(raw).unwrap();
        let back = decode(&out.wire).unwrap();
        assert_eq!(back, raw);
        assert!(back.contains("/srv/app_29/cfg.yaml"));
        assert!(back.contains("500"));
    }

    #[test]
    fn single_line_is_not_templated() {
        assert!(try_template("one line only").is_none());
    }

    #[test]
    fn blocks_containing_the_slot_byte_are_refused() {
        assert!(try_template("a\u{0}b\nc\u{0}d").is_none());
    }

    #[test]
    fn lines_with_tabs_survive() {
        let raw = "col1\t12\tcol3\ncol1\t34\tcol3";
        let out = try_template(raw).unwrap();
        assert_eq!(decode(&out.wire).unwrap(), raw);
    }
}
