//! Native file offsets stay 64-bit on Vita; archives use the ordinary Read/Seek
//! interfaces and share one file choice with distribution loading. The unique
//! handle tracks every read/seek; no mutable native-handle escape is exposed.
#[cfg(target_os = "vita")]
use pvr_psp2_sys::platform::fs::File as PlatformFile;
#[cfg(target_os = "vita")]
pub(crate) use pvr_psp2_sys::platform::fs::Metadata;
#[cfg(all(target_os = "vita", any(feature = "vita", test)))]
pub(crate) use pvr_psp2_sys::platform::fs::metadata;
#[cfg(not(target_os = "vita"))]
use std::fs::File as PlatformFile;
#[cfg(not(target_os = "vita"))]
pub(crate) use std::fs::Metadata;
#[cfg(all(not(target_os = "vita"), any(feature = "vita", test)))]
pub(crate) use std::fs::metadata;
use std::{
    io::{self, Read, Seek, SeekFrom},
    path::Path,
};

pub(crate) type File = Positioned<PlatformFile>;
pub(crate) struct Positioned<R> {
    inner: R,
    position: Option<u64>,
    #[cfg(test)]
    native_seeks: usize,
}
impl<R> Positioned<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            position: Some(0),
            #[cfg(test)]
            native_seeks: 0,
        }
    }
    #[cfg(test)]
    pub(super) fn seek_calls(&self) -> usize {
        self.native_seeks
    }
}
impl Positioned<PlatformFile> {
    pub(crate) fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        PlatformFile::open(path.as_ref()).map(Self::new)
    }
    pub(crate) fn metadata(&self) -> io::Result<Metadata> {
        self.inner.metadata()
    }
}
impl<R: Read> Read for Positioned<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        match self.inner.read(output) {
            Ok(count) => {
                self.position = self
                    .position
                    .and_then(|position| position.checked_add(count as u64));
                Ok(count)
            }
            Err(error) => {
                self.position = None;
                Err(error)
            }
        }
    }
}
impl<R: Seek> Seek for Positioned<R> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        if let SeekFrom::Start(position) = from
            && self.position == Some(position)
        {
            return Ok(position);
        }
        #[cfg(test)]
        {
            self.native_seeks += 1;
        }
        match self.inner.seek(from) {
            Ok(position) => {
                self.position = Some(position);
                Ok(position)
            }
            Err(error) => {
                self.position = None;
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fallible {
        cursor: io::Cursor<Vec<u8>>,
        fail_read: bool,
        fail_seek: bool,
    }
    impl Read for Fallible {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if self.fail_read {
                self.fail_read = false;
                self.cursor.set_position(7);
                return Err(io::ErrorKind::Other.into());
            }
            self.cursor.read(output)
        }
    }
    impl Seek for Fallible {
        fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
            if self.fail_seek {
                self.fail_seek = false;
                self.cursor.set_position(9);
                return Err(io::ErrorKind::Other.into());
            }
            self.cursor.seek(from)
        }
    }
    #[test]
    fn unique_position_tracks_interleaving_short_reads_and_failures() {
        let mut file = Positioned::new(Fallible {
            cursor: io::Cursor::new((0..16).collect()),
            fail_read: false,
            fail_seek: false,
        });
        let mut bytes = [0; 4];
        file.seek(SeekFrom::Start(0)).unwrap();
        file.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, [0, 1, 2, 3]);
        file.seek(SeekFrom::Start(4)).unwrap();
        assert_eq!(file.seek_calls(), 0);
        file.seek(SeekFrom::Start(12)).unwrap();
        file.read_exact(&mut bytes).unwrap();
        file.seek(SeekFrom::Start(4)).unwrap();
        file.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, [4, 5, 6, 7]);
        assert_eq!(file.seek_calls(), 2);
        file.inner.fail_read = true;
        assert!(file.read(&mut bytes).is_err());
        file.seek(SeekFrom::Start(8)).unwrap();
        assert_eq!(file.seek_calls(), 3);
        file.inner.fail_seek = true;
        assert!(file.seek(SeekFrom::Start(10)).is_err());
        file.seek(SeekFrom::Start(8)).unwrap();
        assert_eq!(file.seek_calls(), 5);
        file.seek(SeekFrom::End(-2)).unwrap();
        assert_eq!(file.read(&mut bytes).unwrap(), 2);
        assert_eq!(&bytes[..2], &[14, 15]);
        file.seek(SeekFrom::Start(16)).unwrap();
        assert_eq!(file.read(&mut bytes).unwrap(), 0);
        assert_eq!(file.seek_calls(), 6);
    }

    #[test]
    #[ignore = "reads an existing RPK entry; set REEL_RPK_PATH"]
    fn real_rpk_positioned_file_preserves_uncontended_native_seek_cost() {
        use reel_package::archive::{ArchiveIndex, BLOCK_BYTES, ReadScratch};
        let path = std::env::var("REEL_RPK_PATH").expect("set REEL_RPK_PATH");
        let index = ArchiveIndex::read(&mut std::fs::File::open(&path).unwrap()).unwrap();
        let entry = index
            .entries()
            .filter(|entry| entry.path.ends_with(".png"))
            .max_by_key(|entry| entry.length)
            .unwrap();
        let mut output = vec![0; entry.length as usize];
        for continued in [false, true] {
            let mut durations = Vec::new();
            for _ in 0..7 {
                let mut file = File::open(&path).unwrap();
                let mut scratch = ReadScratch::new();
                let mut cursor = entry.read_cursor(0, output.len()).unwrap();
                let start = std::time::Instant::now();
                if continued {
                    for (block, slice) in output.chunks_mut(BLOCK_BYTES).enumerate() {
                        entry
                            .read_range_with_cursor(
                                &mut file,
                                (block * BLOCK_BYTES) as u64,
                                slice,
                                &mut scratch,
                                &mut cursor,
                                || false,
                            )
                            .unwrap();
                    }
                } else {
                    entry
                        .read_range(&mut file, 0, &mut output, &mut scratch, || false)
                        .unwrap();
                }
                durations.push(start.elapsed().as_micros());
                assert_eq!(blake3::hash(&output).as_bytes(), &entry.version);
                assert_eq!(file.seek_calls(), 2);
            }
            durations.sort_unstable();
            eprintln!(
                "positioned RPK {path} {:?} logical={} stored={} continued={continued} median_us={} native_seeks=2 payload_copies=0",
                entry.path, entry.length, entry.stored_length, durations[3]
            );
        }
    }
}
