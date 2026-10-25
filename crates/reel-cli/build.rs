//! Fingerprint converter implementations independently of the CLI executable.
use std::{
    collections::BTreeSet,
    env, fs, io,
    path::{Path, PathBuf},
};

fn main() -> io::Result<()> {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").map_err(io::Error::other)?);
    let root = manifest.join("../..").canonicalize()?;
    let host = env::var("HOST").map_err(io::Error::other)?;
    let target = env::var("TARGET").map_err(io::Error::other)?;
    println!("cargo:rerun-if-env-changed=HOST");
    println!("cargo:rerun-if-env-changed=TARGET");

    let texture = [
        "crates/reel-cli/src/prepare_texture.rs",
        "crates/reel-core/src/texture.rs",
    ];
    let mut tiles = texture.to_vec();
    tiles.extend([
        "crates/reel-cli/src/prepare_image.rs",
        "crates/reel-core/src/prepared_image.rs",
    ]);
    let opus = [
        "crates/reel-media/src",
        "crates/reel-core/src/audio.rs",
        "crates/reel-audio/src/frame.rs",
    ];
    let video = ["crates/reel-cli/src/prepare_video.rs"];
    let font = ["crates/reel-cli/src/prepare_font.rs"];
    let mut watched = BTreeSet::new();
    for (kind, sources) in [
        ("TEXTURE", texture.as_slice()),
        ("TILES", tiles.as_slice()),
        ("OPUS", opus.as_slice()),
        ("VIDEO", video.as_slice()),
        ("FONT", font.as_slice()),
    ] {
        let mut files = BTreeSet::new();
        collect(&root.join("Cargo.lock"), &mut files, &mut watched)?;
        for source in sources {
            collect(&root.join(source), &mut files, &mut watched)?;
        }
        let mut hash = blake3::Hasher::new();
        field(&mut hash, b"Reel converter implementation v1");
        field(&mut hash, kind.as_bytes());
        field(&mut hash, host.as_bytes());
        field(&mut hash, target.as_bytes());
        for path in files {
            // Checkout locations do not affect generated payloads. Names and
            // contents are length-delimited, ordered and relative to the workspace.
            let relative = path.strip_prefix(&root).map_err(io::Error::other)?;
            let name = relative.to_string_lossy().replace('\\', "/");
            field(&mut hash, name.as_bytes());
            field(&mut hash, &fs::read(path)?);
        }
        println!(
            "cargo:rustc-env=REEL_CONVERTER_{kind}={}",
            hash.finalize().to_hex()
        );
    }
    // Directory watches also detect newly added media implementation files.
    for path in watched {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    Ok(())
}

fn collect(
    path: &Path,
    files: &mut BTreeSet<PathBuf>,
    watched: &mut BTreeSet<PathBuf>,
) -> io::Result<()> {
    watched.insert(path.to_owned());
    let metadata = fs::metadata(path)?;
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            collect(&entry?.path(), files, watched)?;
        }
    } else if metadata.is_file() {
        files.insert(path.to_owned());
    } else {
        return Err(io::Error::other(format!(
            "converter identity input is not a regular file or directory: {}",
            path.display()
        )));
    }
    Ok(())
}

fn field(hash: &mut blake3::Hasher, bytes: &[u8]) {
    hash.update(&(bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}
