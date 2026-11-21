//! Sparse authored voice paths after the self-delimiting typed graph.
use crate::reading::HistoryLog;
use serde::{Serialize, Serializer, ser::SerializeSeq};
use std::io::Write;

const TAG: &[u8] = b"VOX1";

struct Paths<'a> {
    history: &'a HistoryLog,
    count: usize,
}

impl Serialize for Paths<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut entries = serializer.serialize_seq(Some(self.count))?;
        for path in self.history.voice_paths() {
            entries.serialize_element(&path)?;
        }
        entries.end()
    }
}

pub(super) fn write(
    history: &HistoryLog,
    writer: &mut impl Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let count = history.voice_paths().count();
    if count != 0 {
        writer.write_all(TAG)?;
        postcard::to_io(&Paths { history, count }, writer)?;
    }
    Ok(())
}

pub(super) fn read(
    history: &mut HistoryLog,
    bytes: &[u8],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let tail = bytes
        .strip_prefix(TAG)
        .ok_or("unknown checkpoint extension")?;
    let (entries, remaining): (Vec<(u64, usize, &str)>, _) = postcard::take_from_bytes(tail)?;
    if !remaining.is_empty() {
        return Err("unexpected data after checkpoint voice paths".into());
    }
    for (group, index, path) in entries {
        if !history.restore_voice(group, index, path) {
            return Err("voice history line missing or repeated".into());
        }
    }
    Ok(())
}
