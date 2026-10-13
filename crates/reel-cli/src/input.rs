use std::{io::Read, path::Path};

#[derive(Debug)]
pub struct InputError {
    pub code: &'static str,
    pub message: String,
    /// Raw input byte range; UTF-8 positions are unavailable for invalid bytes.
    pub byte_range: Option<std::ops::Range<usize>>,
    pub exit_code: u8,
}

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for InputError {}

fn io_error(action: &str, error: std::io::Error) -> InputError {
    InputError {
        code: "REEL-INPUT-IO",
        message: format!("cannot {action} source: {error}"),
        byte_range: None,
        exit_code: 2,
    }
}
fn not_regular() -> InputError {
    InputError {
        code: "REEL-INPUT-FILE",
        message: "source must be a regular file".into(),
        byte_range: None,
        exit_code: 2,
    }
}
/// Read author source; representability and compiler resources are checked by
/// the shared source pipeline rather than a separate CLI file-size policy.
pub fn read_source(path: &Path) -> Result<String, InputError> {
    let metadata = std::fs::metadata(path).map_err(|e| io_error("inspect", e))?;
    if !metadata.is_file() {
        return Err(not_regular());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A regular file swapped for a FIFO between stat/open must not block.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| io_error("open", e))?;
    if !file
        .metadata()
        .map_err(|e| io_error("inspect open", e))?
        .is_file()
    {
        return Err(not_regular());
    }
    read_utf8(file)
}

fn read_utf8(mut reader: impl Read) -> Result<String, InputError> {
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| io_error("read", e))?;
    String::from_utf8(bytes).map_err(|error| {
        let start = error.utf8_error().valid_up_to();
        let end = error
            .utf8_error()
            .error_len()
            .map_or(error.as_bytes().len(), |len| start + len);
        InputError {
            code: "REEL-INPUT-UTF8",
            message: format!("source is not UTF-8 at byte {start}"),
            byte_range: Some(start..end),
            exit_code: 1,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn read_utf8_preserves_bytes_and_reports_invalid_sequences() {
        let error = read_utf8(&[b'a', 0xe4, 0xb8][..]).unwrap_err();
        assert_eq!(error.byte_range, Some(1..3));
        assert_eq!(error.exit_code, 1);
        let source = "文".repeat(3 * 1024 * 1024);
        assert_eq!(read_utf8(source.as_bytes()).unwrap(), source);
    }
}
