use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewKind {
    Json,
    JsonLines,
    Toml,
    Yaml,
    Xml,
    Csv,
    Markdown,
    Log,
    Code,
    PlainText,
    Binary,
}

impl PreviewKind {
    pub fn label(self) -> &'static str {
        match self {
            PreviewKind::Json => "JSON",
            PreviewKind::JsonLines => "JSONL",
            PreviewKind::Toml => "TOML",
            PreviewKind::Yaml => "YAML",
            PreviewKind::Xml => "XML/HTML",
            PreviewKind::Csv => "CSV",
            PreviewKind::Markdown => "Markdown",
            PreviewKind::Log => "Log",
            PreviewKind::Code => "Codice",
            PreviewKind::PlainText => "Testo",
            PreviewKind::Binary => "Binario (Hex)",
        }
    }
}

pub struct BuiltPreview {
    pub kind: PreviewKind,
    pub formatted: bool,
    pub lines: Vec<Line<'static>>,
}

pub fn build_preview(
    file_name: &str,
    bytes: &[u8],
    pretty_enabled: bool,
) -> BuiltPreview {
    if bytes.is_empty() {
        return BuiltPreview {
            kind: PreviewKind::PlainText,
            formatted: false,
            lines: vec![Line::from(Span::styled(
                "(File vuoto)",
                Style::default().fg(Color::DarkGray),
            ))],
        };
    }

    if is_binary_bytes(bytes) {
        return BuiltPreview {
            kind: PreviewKind::Binary,
            formatted: false,
            lines: build_hex_lines(bytes),
        };
    }

    let raw_text = String::from_utf8_lossy(bytes);
    let ext = file_name
        .rsplit('.')
        .next()
        .filter(|_| file_name.contains('.'))
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    let kind = detect_kind(&ext, &raw_text);

    let (display_text, formatted) = if pretty_enabled {
        match kind {
            PreviewKind::Json => match pretty_json(&raw_text) {
                Some(pretty) => (pretty, true),
                None => (raw_text.into_owned(), false),
            },
            PreviewKind::JsonLines => (pretty_jsonl(&raw_text), true),
            PreviewKind::Xml => match pretty_xml_if_minified(&raw_text) {
                Some(pretty) => (pretty, true),
                None => (raw_text.into_owned(), false),
            },
            PreviewKind::Csv => (format_csv_table(&raw_text, ext == "tsv"), true),
            _ => (raw_text.into_owned(), false),
        }
    } else {
        (raw_text.into_owned(), false)
    };

    let lines = highlight_lines(&display_text, kind, formatted);

    BuiltPreview {
        kind,
        formatted,
        lines,
    }
}

fn is_binary_bytes(bytes: &[u8]) -> bool {
    let sample_len = bytes.len().min(4096);
    let sample = &bytes[..sample_len];
    let has_nul = sample.contains(&0);
    let non_text = sample
        .iter()
        .filter(|&&b| b < 0x09 || (b > 0x0d && b < 0x20))
        .count();
    has_nul || (non_text * 10 > sample_len)
}

fn detect_kind(ext: &str, text: &str) -> PreviewKind {
    match ext {
        "json" | "jsonc" => return PreviewKind::Json,
        "jsonl" | "ndjson" => return PreviewKind::JsonLines,
        "toml" | "ini" | "cfg" | "conf" | "env" => return PreviewKind::Toml,
        "yml" | "yaml" => return PreviewKind::Yaml,
        "xml" | "html" | "htm" | "svg" | "xhtml" | "plist" => return PreviewKind::Xml,
        "csv" | "tsv" => return PreviewKind::Csv,
        "md" | "markdown" => return PreviewKind::Markdown,
        "log" => return PreviewKind::Log,
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "go" | "java" | "c" | "h" | "cpp" | "hpp"
        | "cs" | "rb" | "php" | "sh" | "bash" | "zsh" | "sql" | "css" | "scss" | "swift"
        | "kt" | "dockerfile" => return PreviewKind::Code,
        _ => {}
    }

    let trimmed = text.trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
    {
        return PreviewKind::Json;
    }

    // Check NDJSON (at least 2 non-empty lines, first two parse as JSON objects)
    let mut non_empty = text.lines().map(str::trim).filter(|l| !l.is_empty());
    if let (Some(l1), Some(l2)) = (non_empty.next(), non_empty.next())
        && l1.starts_with('{')
        && l2.starts_with('{')
        && serde_json::from_str::<serde_json::Value>(l1).is_ok()
        && serde_json::from_str::<serde_json::Value>(l2).is_ok()
    {
        return PreviewKind::JsonLines;
    }

    if trimmed.starts_with("<?xml") || trimmed.starts_with("<!DOCTYPE") || trimmed.starts_with("<html") {
        return PreviewKind::Xml;
    }

    PreviewKind::PlainText
}

fn pretty_json(raw: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    serde_json::to_string_pretty(&value).ok()
}

fn pretty_jsonl(raw: &str) -> String {
    let mut out = String::new();
    for (idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if idx > 0 {
            out.push('\n');
        }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed)
            && let Ok(pretty) = serde_json::to_string_pretty(&val)
        {
            out.push_str(&pretty);
            out.push('\n');
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn pretty_xml_if_minified(raw: &str) -> Option<String> {
    let line_count = raw.lines().count();
    if line_count > 3 && raw.len() / line_count.max(1) < 180 {
        return None;
    }
    let mut out = String::with_capacity(raw.len() + 128);
    let mut indent: usize = 0;
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '<' {
            let start = i;
            while i < chars.len() && chars[i] != '>' {
                i += 1;
            }
            if i < chars.len() {
                i += 1;
            }
            let tag: String = chars[start..i].iter().collect();
            let is_closing = tag.starts_with("</");
            let is_self_closing =
                tag.ends_with("/>") || tag.starts_with("<?") || tag.starts_with("<!");

            if is_closing {
                indent = indent.saturating_sub(1);
            }
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            for _ in 0..indent {
                out.push_str("  ");
            }
            out.push_str(&tag);
            out.push('\n');

            if !is_closing && !is_self_closing {
                indent += 1;
            }
        } else {
            let start = i;
            while i < chars.len() && chars[i] != '<' {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                for _ in 0..indent {
                    out.push_str("  ");
                }
                out.push_str(trimmed);
                out.push('\n');
            }
        }
    }

    Some(out)
}

fn format_csv_table(raw: &str, is_tsv: bool) -> String {
    let delim = if is_tsv { '\t' } else { ',' };
    let rows: Vec<Vec<String>> = raw
        .lines()
        .take(500)
        .map(|line| parse_csv_row(line, delim))
        .collect();
    if rows.is_empty() {
        return raw.to_string();
    }
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; cols];
    for row in &rows {
        for (c, cell) in row.iter().enumerate() {
            widths[c] = widths[c].max(cell.chars().count()).min(36);
        }
    }

    let mut out = String::new();
    for (r_idx, row) in rows.iter().enumerate() {
        for c in 0..cols {
            let cell = row.get(c).map(String::as_str).unwrap_or("");
            let truncated: String = if cell.chars().count() > 36 {
                let mut s: String = cell.chars().take(35).collect();
                s.push('…');
                s
            } else {
                cell.to_string()
            };
            if c > 0 {
                out.push_str(" │ ");
            }
            out.push_str(&format!("{:<width$}", truncated, width = widths[c]));
        }
        out.push('\n');
        if r_idx == 0 && rows.len() > 1 {
            for (c, &w) in widths.iter().enumerate() {
                if c > 0 {
                    out.push_str("─┼─");
                }
                out.push_str(&"─".repeat(w));
            }
            out.push('\n');
        }
    }
    out
}

fn parse_csv_row(line: &str, delim: char) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_quotes {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    cur.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                cur.push(ch);
            }
        } else if ch == '"' {
            in_quotes = true;
        } else if ch == delim {
            cells.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
    }
    cells.push(cur);
    cells
}

fn highlight_lines(text: &str, kind: PreviewKind, formatted: bool) -> Vec<Line<'static>> {
    text.lines()
        .enumerate()
        .map(|(idx, line)| {
            let expanded = line.replace('\t', "    ");
            let mut spans = vec![Span::styled(
                format!("{:>4} │ ", idx + 1),
                Style::default().fg(Color::DarkGray),
            )];

            match kind {
                PreviewKind::Json | PreviewKind::JsonLines => {
                    spans.extend(highlight_json_line(&expanded));
                }
                PreviewKind::Toml => {
                    spans.extend(highlight_toml_line(&expanded));
                }
                PreviewKind::Yaml => {
                    spans.extend(highlight_yaml_line(&expanded));
                }
                PreviewKind::Xml => {
                    spans.extend(highlight_xml_line(&expanded));
                }
                PreviewKind::Csv if formatted => {
                    let style = if idx == 0 {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else if idx == 1 {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    spans.push(Span::styled(expanded, style));
                }
                PreviewKind::Markdown => {
                    spans.extend(highlight_markdown_line(&expanded));
                }
                PreviewKind::Log => {
                    spans.extend(highlight_log_line(&expanded));
                }
                PreviewKind::Code => {
                    spans.extend(highlight_code_line(&expanded));
                }
                _ => {
                    spans.push(Span::styled(expanded, Style::default().fg(Color::White)));
                }
            }

            Line::from(spans)
        })
        .collect()
}

fn highlight_json_line(line: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];
        if ch.is_whitespace() {
            let start = i;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            spans.push(Span::raw(chars[start..i].iter().collect::<String>()));
        } else if ch == '"' {
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 2;
                    continue;
                }
                if chars[i] == '"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            let token: String = chars[start..i].iter().collect();
            // Look ahead for ':' to determine if this string is a JSON key
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let is_key = j < chars.len() && chars[j] == ':';
            let style = if is_key {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Green)
            };
            spans.push(Span::styled(token, style));
        } else if ch == '-' || ch.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_digit()
                    || matches!(chars[i], '.' | 'e' | 'E' | '+' | '-'))
            {
                i += 1;
            }
            let token: String = chars[start..i].iter().collect();
            spans.push(Span::styled(token, Style::default().fg(Color::Yellow)));
        } else if ch.is_ascii_alphabetic() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            let token: String = chars[start..i].iter().collect();
            let style = match token.as_str() {
                "true" | "false" => Style::default().fg(Color::Magenta),
                "null" => Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::ITALIC),
                _ => Style::default().fg(Color::White),
            };
            spans.push(Span::styled(token, style));
        } else {
            let style = match ch {
                '{' | '}' | '[' | ']' => Style::default().fg(Color::Gray),
                ':' | ',' => Style::default().fg(Color::DarkGray),
                _ => Style::default().fg(Color::White),
            };
            spans.push(Span::styled(ch.to_string(), style));
            i += 1;
        }
    }

    spans
}

fn highlight_toml_line(line: &str) -> Vec<Span<'static>> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') || trimmed.starts_with(';') {
        return vec![Span::styled(
            line.to_string(),
            Style::default().fg(Color::DarkGray),
        )];
    }
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        return vec![Span::styled(
            line.to_string(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )];
    }
    if let Some(eq) = line.find('=') {
        let key = line[..eq].to_string();
        let val = line[eq + 1..].to_string();
        let val_trimmed = val.trim();
        let val_style = if val_trimmed.starts_with('"') || val_trimmed.starts_with('\'') {
            Style::default().fg(Color::Green)
        } else if matches!(val_trimmed, "true" | "false") {
            Style::default().fg(Color::Magenta)
        } else if val_trimmed.parse::<f64>().is_ok() {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::White)
        };
        return vec![
            Span::styled(key, Style::default().fg(Color::Cyan)),
            Span::styled("=", Style::default().fg(Color::DarkGray)),
            Span::styled(val, val_style),
        ];
    }
    vec![Span::styled(line.to_string(), Style::default().fg(Color::White))]
}

fn highlight_yaml_line(line: &str) -> Vec<Span<'static>> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return vec![Span::styled(
            line.to_string(),
            Style::default().fg(Color::DarkGray),
        )];
    }
    if let Some(colon) = line.find(':') {
        let key = line[..colon].to_string();
        let rest = line[colon + 1..].to_string();
        let val_trimmed = rest.trim();
        let val_style = if val_trimmed.starts_with('"') || val_trimmed.starts_with('\'') {
            Style::default().fg(Color::Green)
        } else if matches!(val_trimmed, "true" | "false" | "null" | "~") {
            Style::default().fg(Color::Magenta)
        } else if val_trimmed.parse::<f64>().is_ok() {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::White)
        };
        return vec![
            Span::styled(key, Style::default().fg(Color::Cyan)),
            Span::styled(":", Style::default().fg(Color::DarkGray)),
            Span::styled(rest, val_style),
        ];
    }
    vec![Span::styled(line.to_string(), Style::default().fg(Color::White))]
}

fn highlight_xml_line(line: &str) -> Vec<Span<'static>> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("<!--") {
        return vec![Span::styled(
            line.to_string(),
            Style::default().fg(Color::DarkGray),
        )];
    }
    let mut spans = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<' {
            let start = i;
            while i < chars.len() && chars[i] != '>' {
                i += 1;
            }
            if i < chars.len() {
                i += 1;
            }
            let tag: String = chars[start..i].iter().collect();
            spans.push(Span::styled(tag, Style::default().fg(Color::LightBlue)));
        } else {
            let start = i;
            while i < chars.len() && chars[i] != '<' {
                i += 1;
            }
            let txt: String = chars[start..i].iter().collect();
            spans.push(Span::styled(txt, Style::default().fg(Color::White)));
        }
    }
    spans
}

fn highlight_markdown_line(line: &str) -> Vec<Span<'static>> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        vec![Span::styled(
            line.to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )]
    } else if trimmed.starts_with("```") {
        vec![Span::styled(
            line.to_string(),
            Style::default().fg(Color::Green),
        )]
    } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
        vec![Span::styled(
            line.to_string(),
            Style::default().fg(Color::Yellow),
        )]
    } else {
        vec![Span::styled(line.to_string(), Style::default().fg(Color::White))]
    }
}

fn highlight_log_line(line: &str) -> Vec<Span<'static>> {
    let upper = line.to_ascii_uppercase();
    let style = if upper.contains("ERROR") || upper.contains("FATAL") || upper.contains("PANIC") {
        Style::default().fg(Color::Red)
    } else if upper.contains("WARN") {
        Style::default().fg(Color::Yellow)
    } else if upper.contains("DEBUG") || upper.contains("TRACE") {
        Style::default().fg(Color::DarkGray)
    } else if upper.contains("INFO") {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::White)
    };
    vec![Span::styled(line.to_string(), style)]
}

fn highlight_code_line(line: &str) -> Vec<Span<'static>> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with("--") {
        return vec![Span::styled(
            line.to_string(),
            Style::default().fg(Color::DarkGray),
        )];
    }

    const KEYWORDS: &[&str] = &[
        "fn", "pub", "let", "mut", "const", "struct", "enum", "impl", "trait", "use", "mod",
        "return", "if", "else", "match", "for", "while", "loop", "async", "await", "def",
        "class", "import", "from", "function", "var", "export", "default", "interface", "type",
        "SELECT", "FROM", "WHERE", "INSERT", "UPDATE", "DELETE", "JOIN", "CREATE", "TABLE",
    ];

    let mut spans = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];
        if ch == '"' || ch == '\'' {
            let quote = ch;
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            let s: String = chars[start..i].iter().collect();
            spans.push(Span::styled(s, Style::default().fg(Color::Green)));
        } else if ch.is_ascii_alphabetic() || ch == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if KEYWORDS.contains(&word.as_str()) {
                spans.push(Span::styled(
                    word,
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled(word, Style::default().fg(Color::White)));
            }
        } else if ch.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let num: String = chars[start..i].iter().collect();
            spans.push(Span::styled(num, Style::default().fg(Color::Yellow)));
        } else {
            let start = i;
            i += 1;
            while i < chars.len()
                && !chars[i].is_ascii_alphanumeric()
                && chars[i] != '_'
                && chars[i] != '"'
                && chars[i] != '\''
            {
                i += 1;
            }
            let sym: String = chars[start..i].iter().collect();
            spans.push(Span::styled(sym, Style::default().fg(Color::Gray)));
        }
    }

    spans
}

fn build_hex_lines(bytes: &[u8]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let preview_len = bytes.len().min(4096);
    for (idx, chunk) in bytes[..preview_len].chunks(16).enumerate() {
        let offset = idx * 16;
        let mut spans = vec![Span::styled(
            format!("{offset:08x}  "),
            Style::default().fg(Color::DarkGray),
        )];

        let mut ascii_part = String::with_capacity(16);
        for i in 0..16 {
            if i == 8 {
                spans.push(Span::raw(" "));
            }
            if let Some(&b) = chunk.get(i) {
                let style = if b == 0 {
                    Style::default().fg(Color::DarkGray)
                } else if (0x20..=0x7e).contains(&b) {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default().fg(Color::Yellow)
                };
                spans.push(Span::styled(format!("{b:02x} "), style));
                ascii_part.push(if (0x20..=0x7e).contains(&b) {
                    b as char
                } else {
                    '.'
                });
            } else {
                spans.push(Span::raw("   "));
                ascii_part.push(' ');
            }
        }
        spans.push(Span::styled(" |", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(ascii_part, Style::default().fg(Color::Green)));
        spans.push(Span::styled("|", Style::default().fg(Color::DarkGray)));
        lines.push(Line::from(spans));
    }

    if bytes.len() > preview_len {
        lines.push(Line::from(Span::styled(
            format!(
                "... ({} byte totali caricati per l'anteprima binaria)",
                bytes.len()
            ),
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_auto_pretty_print() {
        let raw = br#"{"name":"kftp","port":22,"nested":{"ok":true}}"#;
        let built = build_preview("config.json", raw, true);
        assert_eq!(built.kind, PreviewKind::Json);
        assert!(built.formatted);
        assert!(built.lines.len() >= 6);
    }

    #[test]
    fn test_json_raw_mode() {
        let raw = br#"{"a":1,"b":2}"#;
        let built = build_preview("data.json", raw, false);
        assert_eq!(built.kind, PreviewKind::Json);
        assert!(!built.formatted);
        assert_eq!(built.lines.len(), 1);
    }

    #[test]
    fn test_csv_table_format() {
        let raw = b"name,size\nfoo.txt,12\nbar.json,4096\n";
        let built = build_preview("items.csv", raw, true);
        assert_eq!(built.kind, PreviewKind::Csv);
        assert!(built.formatted);
        // header + separator + 2 data rows
        assert_eq!(built.lines.len(), 4);
    }

    #[test]
    fn test_binary_detection() {
        let raw = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00];
        let built = build_preview("image.png", raw, true);
        assert_eq!(built.kind, PreviewKind::Binary);
        assert!(!built.lines.is_empty());
    }
}
