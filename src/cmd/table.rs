use unicode_width::UnicodeWidthStr;

pub fn render(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths = headers
        .iter()
        .map(|value| UnicodeWidthStr::width(*value))
        .collect::<Vec<_>>();
    for row in rows {
        for (index, value) in row.iter().enumerate().take(widths.len()) {
            widths[index] = widths[index].max(UnicodeWidthStr::width(value.as_str()));
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
            let padding = widths[index].saturating_sub(UnicodeWidthStr::width(*value)) + 2;
            output.push_str(&" ".repeat(padding));
        }
    }
    output.push('\n');
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
}
