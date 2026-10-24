//! SDK newlib 5f6d38c8 replaces by unlinking the destination before rename.
//! Use the kernel's no-replace operation and its original SCE error source.
//! Source: https://github.com/vitasdk/newlib/blob/5f6d38c8e77d53be0e36065bbb2527512bf19d5e/newlib/libc/sys/vita/syscalls.c#L374
//! File::sync_all uses fsync -> sceIoSyncByFd in the matching fs.c:205.
//! Mount sync contract: https://github.com/vitasdk/vita-headers/blob/a1bc71a05f503641ab2ec226b0ee9fa37e3977e8/include/psp2/io/fcntl.h
use super::{Published, generations};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

struct Native;
impl generations::Kernel for Native {
    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        ffi::rename_no_replace(from, to)
    }
    fn sync_mount(&self, path: &Path) -> io::Result<()> {
        ffi::sync_mount(path)
    }
}

pub(super) fn publish(
    target: &Path,
    overwrite: bool,
    fill: impl FnOnce(&mut dyn io::Write) -> io::Result<()>,
) -> io::Result<Published> {
    generations::publish(target, overwrite, fill, &Native)
}
pub(super) fn open(target: &Path) -> io::Result<fs::File> {
    generations::open(target)
}
pub(super) fn remove(target: &Path) -> io::Result<Published> {
    generations::remove(target, &Native)
}
pub(super) fn entries(
    directory: &Path,
) -> io::Result<impl Iterator<Item = io::Result<PathBuf>> + use<>> {
    generations::entries(directory)
}

// Native calls alone cross this boundary. The storage protocol and owned File
// values stay safe Rust. Pinned SDK behavior is recorded in the module contract.
#[allow(unsafe_code)]
mod ffi {
    use std::{
        ffi::{CString, c_char},
        fmt, io,
        os::unix::ffi::OsStrExt,
        path::Path,
    };

    #[link(name = "SceIofilemgr_stub", kind = "static")]
    unsafe extern "C" {
        fn sceIoRename(from: *const c_char, to: *const c_char) -> i32;
        fn sceIoSync(device: *const c_char, flags: u32) -> i32;
    }
    unsafe extern "C" {
        fn __vita_sce_errno_to_errno(code: i32, kind: i32) -> i32;
    }

    #[derive(Debug)]
    struct NativeError {
        operation: &'static str,
        code: i32,
        errno: i32,
    }
    impl fmt::Display for NativeError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                f,
                "{} failed (SCE {:#010x}, errno {})",
                self.operation, self.code, self.errno
            )
        }
    }
    impl std::error::Error for NativeError {}

    fn checked(operation: &'static str, code: i32) -> io::Result<()> {
        if code >= 0 {
            return Ok(());
        }
        // SAFETY: newlib's pure SDK error conversion accepts the returned SCE
        // error and ERROR_GENERIC (0), preserving its native code in the source.
        let errno = unsafe { __vita_sce_errno_to_errno(code, 0) };
        Err(io::Error::new(
            io::Error::from_raw_os_error(errno).kind(),
            NativeError {
                operation,
                code,
                errno,
            },
        ))
    }

    pub(super) fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
        let from = CString::new(from.as_os_str().as_bytes()).map_err(io::Error::other)?;
        let to = CString::new(to.as_os_str().as_bytes()).map_err(io::Error::other)?;
        // SAFETY: both owned C strings remain live through the call; the SDK
        // borrows their NUL-terminated paths and retains neither pointer.
        checked("sceIoRename", unsafe {
            sceIoRename(from.as_ptr(), to.as_ptr())
        })
    }

    pub(super) fn sync_mount(path: &Path) -> io::Result<()> {
        let bytes = path.as_os_str().as_bytes();
        let end = bytes.iter().position(|byte| *byte == b':').ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "native storage path has no device",
            )
        })?;
        let device = CString::new(&bytes[..=end]).map_err(io::Error::other)?;
        // SAFETY: the owned device string remains live; flags 0 requests mount
        // synchronization. The SDK retains no pointer after this synchronous call.
        checked("sceIoSync", unsafe { sceIoSync(device.as_ptr(), 0) })
    }
}
