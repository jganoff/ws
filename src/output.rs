use std::io::Write;

use anyhow::{Result, bail};
use tabwriter::TabWriter;

pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    dest: Box<dyn Write>,
}

impl Table {
    pub fn new(w: Box<dyn Write>, headers: Vec<String>) -> Self {
        Table {
            headers,
            rows: Vec::new(),
            dest: w,
        }
    }

    pub fn add_row(&mut self, columns: Vec<String>) -> Result<()> {
        if columns.len() != self.headers.len() {
            bail!(
                "row has {} columns, expected {}",
                columns.len(),
                self.headers.len()
            );
        }
        self.rows.push(columns);
        Ok(())
    }

    pub fn render(&mut self) -> Result<()> {
        if self.headers.is_empty() {
            return Ok(());
        }

        let buf = render_buf(&self.headers, &self.rows)?;
        self.dest.write_all(&buf)?;
        Ok(())
    }
}

fn render_buf(headers: &[String], rows: &[Vec<String>]) -> Result<Vec<u8>> {
    let mut tw = TabWriter::new(Vec::new()).minwidth(0).padding(2);

    let upper: Vec<String> = headers.iter().map(|h| h.to_uppercase()).collect();
    writeln!(tw, "{}", upper.join("\t"))?;

    for row in rows {
        writeln!(tw, "{}", row.join("\t"))?;
    }

    tw.flush()?;
    Ok(tw.into_inner()?)
}

pub fn format_repo_status(ahead: u32, modified: u32) -> String {
    if ahead == 0 && modified == 0 {
        return "clean".to_string();
    }
    let mut parts = Vec::new();
    if ahead > 0 {
        parts.push(format!("{} ahead", ahead));
    }
    if modified > 0 {
        parts.push(format!("{} modified", modified));
    }
    parts.join(", ")
}

pub fn format_error(err: &dyn std::fmt::Display) -> String {
    format!("ERROR: {}", err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalize_whitespace(s: &str) -> String {
        s.lines()
            .map(|line| line.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_to_string(headers: &[String], rows: &[Vec<String>]) -> String {
        if headers.is_empty() {
            return String::new();
        }
        let buf = render_buf(headers, rows).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn test_table() {
        let cases: Vec<(&str, Vec<&str>, Vec<Vec<&str>>, &str)> = vec![
            (
                "single column",
                vec!["Name"],
                vec![vec!["Alice"], vec!["Bob"]],
                "NAME\nAlice\nBob\n",
            ),
            (
                "two columns aligned",
                vec!["Name", "Status"],
                vec![
                    vec!["api-gateway", "clean"],
                    vec!["user-service", "2 modified"],
                ],
                "NAME          STATUS\napi-gateway   clean\nuser-service  2 modified\n",
            ),
            (
                "three columns",
                vec!["Repository", "Branch", "Status"],
                vec![
                    vec!["api-gateway", "main", "clean"],
                    vec!["user-service", "feature-branch", "2 modified"],
                ],
                "REPOSITORY    BRANCH          STATUS\napi-gateway   main            clean\nuser-service  feature-branch  2 modified\n",
            ),
            (
                "headers only no rows",
                vec!["Name", "Age"],
                vec![],
                "NAME  AGE\n",
            ),
            ("no headers", vec![], vec![], ""),
        ];
        for (name, headers, rows, want) in cases {
            let headers_owned: Vec<String> = headers.iter().map(|s| s.to_string()).collect();
            let rows_owned: Vec<Vec<String>> = rows
                .iter()
                .map(|r| r.iter().map(|s| s.to_string()).collect())
                .collect();
            let output = render_to_string(&headers_owned, &rows_owned);
            assert_eq!(
                normalize_whitespace(&output),
                normalize_whitespace(want),
                "{}",
                name
            );
        }
    }

    #[test]
    fn test_table_column_mismatch() {
        let mut table = Table::new(Box::new(std::io::sink()), vec!["Name".into(), "Age".into()]);

        let err = table.add_row(vec!["Alice".into(), "30".into(), "extra".into()]);
        assert!(err.is_err());
        assert!(
            err.unwrap_err()
                .to_string()
                .contains("3 columns, expected 2")
        );

        let err = table.add_row(vec!["Bob".into()]);
        assert!(err.is_err());
        assert!(
            err.unwrap_err()
                .to_string()
                .contains("1 columns, expected 2")
        );
    }

    #[test]
    fn test_format_repo_status() {
        let cases = vec![
            ("clean", 0, 0, "clean"),
            ("modified only", 0, 5, "5 modified"),
            ("ahead only", 3, 0, "3 ahead"),
            ("both", 2, 4, "2 ahead, 4 modified"),
            ("one each", 1, 1, "1 ahead, 1 modified"),
        ];
        for (name, ahead, modified, want) in cases {
            assert_eq!(format_repo_status(ahead, modified), want, "{}", name);
        }
    }

    #[test]
    fn test_format_error() {
        assert_eq!(format_error(&"something broke"), "ERROR: something broke");
    }
}
