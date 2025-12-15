use anyhow::Result;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

/// Maximum visible characters to show in output preview
pub const MAX_PREVIEW_CHARS: usize = 50;

/// Log directory for task output
pub const LOG_DIR: &str = ".just-fancy-logs";

/// Extract a preview from an output line
///
/// Strips ANSI codes and truncates to max_visible characters.
pub fn extract_preview(line: &str, max_visible: usize) -> String {
    // Strip ANSI escape codes
    let stripped = strip_ansi(line);

    // Trim whitespace
    let trimmed = stripped.trim();

    if trimmed.is_empty() {
        return String::new();
    }

    let chars: Vec<char> = trimmed.chars().collect();

    if chars.len() <= max_visible {
        trimmed.to_string()
    } else {
        let truncated: String = chars.into_iter().take(max_visible).collect();
        format!("{}…", truncated)
    }
}

/// Strip ANSI escape codes from a string
pub fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let stripped = strip_ansi_escapes::strip(bytes);
    String::from_utf8_lossy(&stripped).to_string()
}

/// Output collector that buffers output and tracks the latest line
pub struct OutputCollector {
    /// Full output buffer
    buffer: Vec<u8>,
    /// Latest non-empty line for preview
    latest_line: String,
    /// Recipe name (for log file)
    recipe_name: String,
}

impl OutputCollector {
    /// Create a new output collector for a recipe
    pub fn new(recipe_name: &str) -> Self {
        Self {
            buffer: Vec::new(),
            latest_line: String::new(),
            recipe_name: recipe_name.to_string(),
        }
    }

    /// Append data to the buffer and update latest line
    pub fn append(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);

        // Try to extract latest non-empty line
        if let Ok(text) = std::str::from_utf8(data) {
            for line in text.lines() {
                let stripped = strip_ansi(line);
                let trimmed = stripped.trim();
                if !trimmed.is_empty() {
                    self.latest_line = trimmed.to_string();
                }
            }
        }
    }

    /// Get the latest line preview
    pub fn latest_preview(&self) -> String {
        extract_preview(&self.latest_line, MAX_PREVIEW_CHARS)
    }

    /// Write the full output to a log file
    pub fn write_log(&self) -> Result<()> {
        let log_dir = Path::new(LOG_DIR);
        fs::create_dir_all(log_dir)?;

        let log_path = log_dir.join(format!("{}.log", self.recipe_name));
        let mut file = File::create(&log_path)?;
        file.write_all(&self.buffer)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_preview_short() {
        let preview = extract_preview("hello world", 50);
        assert_eq!(preview, "hello world");
    }

    #[test]
    fn test_extract_preview_long() {
        let long_line = "a".repeat(100);
        let preview = extract_preview(&long_line, 50);
        assert_eq!(preview.chars().count(), 51); // 50 + ellipsis
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn test_extract_preview_with_ansi() {
        let ansi_line = "\x1b[32mgreen text\x1b[0m";
        let preview = extract_preview(ansi_line, 50);
        assert_eq!(preview, "green text");
    }

    #[test]
    fn test_extract_preview_empty() {
        let preview = extract_preview("   ", 50);
        assert_eq!(preview, "");
    }

    #[test]
    fn test_output_collector() {
        let mut collector = OutputCollector::new("test-recipe");
        collector.append(b"first line\n");
        collector.append(b"second line\n");
        assert_eq!(collector.latest_preview(), "second line");
    }

    #[test]
    fn test_output_collector_ignores_blank_lines() {
        let mut collector = OutputCollector::new("test-recipe");
        collector.append(b"meaningful line\n");
        collector.append(b"\n");
        collector.append(b"   \n");
        assert_eq!(collector.latest_preview(), "meaningful line");
    }

    #[test]
    fn test_output_collector_strips_ansi_in_preview() {
        let mut collector = OutputCollector::new("test-recipe");
        collector.append(b"\x1b[32mgreen output\x1b[0m\n");
        assert_eq!(collector.latest_preview(), "green output");
    }

    #[test]
    fn test_extract_preview_truncates_long_ansi() {
        // Long line with ANSI codes
        let long_colored = format!("\x1b[31m{}\x1b[0m", "x".repeat(100));
        let preview = extract_preview(&long_colored, 50);
        assert_eq!(preview.chars().count(), 51); // 50 + ellipsis
        assert!(preview.ends_with('…'));
        assert!(!preview.contains('\x1b')); // No ANSI codes in output
    }

    #[test]
    fn test_strip_ansi() {
        assert_eq!(strip_ansi("\x1b[32mgreen\x1b[0m"), "green");
        assert_eq!(strip_ansi("no codes"), "no codes");
        assert_eq!(
            strip_ansi("\x1b[1;31;40mbold red on black\x1b[0m"),
            "bold red on black"
        );
    }
}
