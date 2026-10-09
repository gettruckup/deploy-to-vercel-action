//! GitHub Actions workflow commands and file commands, matching `@actions/core` 1.10.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::error::Result;

/// Log lines, workflow commands and file commands. `&self` methods so concurrent tasks can share it.
pub struct Io {
    sink: Mutex<Box<dyn Write + Send>>,
    output_path: Option<PathBuf>,
    env_path: Option<PathBuf>,
}

impl Io {
    pub fn new(
        sink: Box<dyn Write + Send>,
        output_path: Option<PathBuf>,
        env_path: Option<PathBuf>,
    ) -> Self {
        Self {
            sink: Mutex::new(sink),
            output_path,
            env_path,
        }
    }

    /// Stdout plus the `GITHUB_OUTPUT` / `GITHUB_ENV` files named by the process environment.
    pub fn from_process_env() -> Self {
        let path = |key: &str| {
            std::env::var_os(key)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Self::new(
            Box::new(std::io::stdout()),
            path("GITHUB_OUTPUT"),
            path("GITHUB_ENV"),
        )
    }

    fn line(&self, text: &str) {
        let mut sink = self
            .sink
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = writeln!(sink, "{text}");
        let _ = sink.flush();
    }

    pub fn info(&self, message: &str) {
        self.line(message);
    }

    pub fn debug(&self, message: &str) {
        self.line(&format!("::debug::{}", escape_data(message)));
    }

    pub fn warning(&self, message: &str) {
        self.line(&format!("::warning::{}", escape_data(message)));
    }

    pub fn error(&self, message: &str) {
        self.line(&format!("::error::{}", escape_data(message)));
    }

    pub fn mask(&self, secret: &str) {
        if !secret.is_empty() {
            self.line(&format!("::add-mask::{}", escape_data(secret)));
        }
    }

    pub fn set_output(&self, name: &str, value: &str) -> Result<()> {
        match &self.output_path {
            Some(path) => append_file_command(path, name, value),
            None => {
                self.line("");
                self.line(&format!(
                    "::set-output name={}::{}",
                    escape_property(name),
                    escape_data(value)
                ));
                Ok(())
            }
        }
    }

    pub fn export_var(&self, name: &str, value: &str) -> Result<()> {
        match &self.env_path {
            Some(path) => append_file_command(path, name, value),
            None => {
                self.line(&format!(
                    "::set-env name={}::{}",
                    escape_property(name),
                    escape_data(value)
                ));
                Ok(())
            }
        }
    }
}

pub fn file_command_entry(name: &str, value: &str, delimiter: &str) -> String {
    format!("{name}<<{delimiter}\n{value}\n{delimiter}\n")
}

fn append_file_command(path: &Path, name: &str, value: &str) -> Result<()> {
    let delimiter = format!("ghadelimiter_{}", uuid::Uuid::new_v4());
    let mut file = OpenOptions::new().append(true).create(true).open(path)?;
    file.write_all(file_command_entry(name, value, &delimiter).as_bytes())?;
    Ok(())
}

/// Parses `GITHUB_OUTPUT` / `GITHUB_ENV` content (heredoc and `name=value` forms).
pub fn parse_file_commands(content: &str) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    let mut lines = content.lines();
    while let Some(line) = lines.next() {
        if let Some((name, delimiter)) = line.split_once("<<") {
            let mut value = Vec::new();
            for next in lines.by_ref() {
                if next == delimiter {
                    break;
                }
                value.push(next);
            }
            entries.push((name.to_string(), value.join("\n")));
        } else if let Some((name, value)) = line.split_once('=') {
            entries.push((name.to_string(), value.to_string()));
        }
    }
    entries
}

pub fn escape_data(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

pub fn escape_property(value: &str) -> String {
    escape_data(value).replace(':', "%3A").replace(',', "%2C")
}

/// In-memory, cloneable `Write` sink for tests.
#[derive(Clone, Default)]
pub struct SharedBuf(pub Arc<Mutex<Vec<u8>>>);

impl SharedBuf {
    pub fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for SharedBuf {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn io_with(output: Option<PathBuf>, env: Option<PathBuf>) -> (Io, SharedBuf) {
        let buf = SharedBuf::default();
        (Io::new(Box::new(buf.clone()), output, env), buf)
    }

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn file_command_entry_matches_actions_core_format() {
        assert_eq!(
            file_command_entry("PREVIEW_URL", "https://a", "ghadelimiter_x"),
            "PREVIEW_URL<<ghadelimiter_x\nhttps://a\nghadelimiter_x\n"
        );
    }

    #[test]
    fn set_output_appends_heredoc_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output");
        let (io, log) = io_with(Some(path.clone()), None);
        io.set_output("A", "1").unwrap();
        io.set_output("B", "two\nlines").unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("A<<ghadelimiter_"));
        assert_eq!(
            parse_file_commands(&content),
            pairs(&[("A", "1"), ("B", "two\nlines")])
        );
        assert_eq!(log.contents(), "");
    }

    #[test]
    fn set_output_falls_back_to_workflow_command() {
        let (io, log) = io_with(None, None);
        io.set_output("A,B", "x\ny%").unwrap();
        assert_eq!(log.contents(), "\n::set-output name=A%2CB::x%0Ay%25\n");
    }

    #[test]
    fn export_var_writes_github_env_or_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("env");
        let (io, _) = io_with(None, Some(path.clone()));
        io.export_var("VERCEL_ORG_ID", "team_1").unwrap();
        assert_eq!(
            parse_file_commands(&fs::read_to_string(&path).unwrap()),
            pairs(&[("VERCEL_ORG_ID", "team_1")])
        );

        let (io, log) = io_with(None, None);
        io.export_var("VERCEL_ORG_ID", "team_1").unwrap();
        assert_eq!(log.contents(), "::set-env name=VERCEL_ORG_ID::team_1\n");
    }

    #[test]
    fn log_commands_escape_data() {
        let (io, log) = io_with(None, None);
        io.info("plain");
        io.debug("a\nb");
        io.warning("50%");
        io.error("bad\r\n");
        io.mask("s3cret");
        io.mask("");
        assert_eq!(
            log.contents(),
            "plain\n::debug::a%0Ab\n::warning::50%25\n::error::bad%0D%0A\n::add-mask::s3cret\n"
        );
    }

    #[test]
    fn parse_file_commands_reads_both_formats() {
        let content = "A<<d1\nx\ny\nd1\nB=plain\n";
        assert_eq!(
            parse_file_commands(content),
            pairs(&[("A", "x\ny"), ("B", "plain")])
        );
    }
}
