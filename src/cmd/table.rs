use unicode_width::UnicodeWidthStr;

pub fn render(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths = headers
        .iter()
        .map(|value| display_width(value))
        .collect::<Vec<_>>();
    for row in rows {
        for (index, value) in row.iter().enumerate().take(widths.len()) {
            widths[index] = widths[index].max(display_width(value));
        }
    }
    let mut output = String::new();
    append_row(&mut output, headers.iter().copied(), &widths);
    for row in rows {
        append_row(&mut output, row.iter().map(String::as_str), &widths);
    }
    output
}

fn append_row<'a>(output: &mut String, values: impl Iterator<Item = &'a str>, widths: &[usize]) {
    let values = values.collect::<Vec<_>>();
    for (index, value) in values.iter().enumerate() {
        output.push_str(value);
        if index + 1 < values.len() {
            let padding = widths[index].saturating_sub(display_width(value)) + 2;
            output.push_str(&" ".repeat(padding));
        }
    }
    output.push('\n');
}

fn display_width(value: &str) -> usize {
    let mut plain = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            if chars.next() == Some('[') {
                for code in chars.by_ref() {
                    if ('@'..='~').contains(&code) {
                        break;
                    }
                }
            }
        } else {
            plain.push(character);
        }
    }
    UnicodeWidthStr::width(plain.as_str())
}

#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn aligns_ascii_and_wide_characters() {
        let table = render(
            &["NAME", "PRODUCT", "STATUS"],
            &[
                vec!["h3c".into(), "新华三".into(), "supported".into()],
                vec!["teleport".into(), "Teleport".into(), "verified".into()],
            ],
        );
        assert_eq!(
            table,
            "NAME      PRODUCT   STATUS\nh3c       新华三    supported\nteleport  Teleport  verified\n"
        );
    }

    #[test]
    fn ignores_ansi_sequences_when_aligning() {
        let table = render(
            &["NAME", "VALUE"],
            &[
                vec!["\u{1b}[36mGPU\u{1b}[0m".into(), "8".into()],
                vec!["memory".into(), "1 TiB".into()],
            ],
        );
        assert!(table.contains("\u{1b}[36mGPU\u{1b}[0m     8"));
    }
}
