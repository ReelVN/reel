//! Update the launcher font in the fixed Vita runtime assets
//! consumed by `reel-player`'s `package.metadata.vita.assets` directory.
//! `cargo +nightly vita build vpk` owns executable conversion and VPK packaging.

use anyhow::{Context, Result, ensure};
use reel_package::archive::{ArchiveKind, ArchiveWriter};
use std::{
    fs,
    io::BufWriter,
    path::{Path, PathBuf},
};

/// Font and redistribution license installed in the Vita launcher archive.
#[derive(clap::Args)]
pub struct Assets {
    #[arg(long)]
    font: PathBuf,
    #[arg(long)]
    font_license: PathBuf,
}

/// Update `crates/reel-player/runtime/vita` for the launcher VPK.
///
/// Arguments are `--font FILE --font-license FILE`. The supplied OpenType or
/// TrueType font (including a TTC) retains its complete logical bytes at
/// `fonts/body.ttf` inside `launcher.rpk`; its content digest is computed during
/// packaging so startup mounts only the archive index before loading the font.
/// The font's redistribution license is copied to `licenses/launcher-font.txt`.
/// LiveArea artwork is maintained beside these inputs and remains unchanged.
pub fn assets(options: Assets) -> Result<()> {
    let font = options.font.as_path();
    let license = options.font_license.as_path();
    for path in [font.to_owned(), license.to_owned()] {
        ensure!(
            fs::metadata(&path)
                .with_context(|| format!("read Vita asset {}", path.display()))?
                .len()
                > 0,
            "empty Vita asset: {}",
            path.display()
        );
    }

    let output = crate::root().join("crates/reel-player/runtime/vita");
    assemble(&output, font, license)?;
    println!("Vita launcher assets: {}", output.display());
    Ok(())
}

fn assemble(output: &Path, font: &Path, license: &Path) -> Result<()> {
    fs::create_dir_all(output.join("licenses"))?;
    let mut archive = ArchiveWriter::new(
        BufWriter::new(fs::File::create(output.join("launcher.rpk"))?),
        ArchiveKind::Resources,
    )?;
    archive.add("fonts/body.ttf", &mut fs::File::open(font)?)?;
    archive.finish()?;
    fs::copy(license, output.join("licenses/launcher-font.txt"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reel_package::archive::{ArchiveIndex, ReadScratch};

    #[test]
    fn launcher_assets_keep_complete_font_in_standard_archive() {
        let root = crate::root()
            .join("target/reel-artifacts/vita-assets-tests")
            .join(std::process::id().to_string());
        let source = root.join("source");
        let output = root.join("output");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&output).unwrap();
        let font = source.join("font.ttc");
        let license = source.join("license.txt");
        let bytes = (0..150_000)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        fs::write(&font, &bytes).unwrap();
        fs::write(&license, b"font redistribution license").unwrap();
        fs::create_dir_all(output.join("sce_sys")).unwrap();
        fs::write(output.join("sce_sys/icon0.png"), b"maintained artwork").unwrap();
        assemble(&output, &font, &license).unwrap();
        assert_eq!(
            fs::read(output.join("sce_sys/icon0.png")).unwrap(),
            b"maintained artwork"
        );
        let mut archive = fs::File::open(output.join("launcher.rpk")).unwrap();
        let index = ArchiveIndex::read(&mut archive).unwrap();
        assert_eq!(index.kind(), ArchiveKind::Resources);
        assert_eq!(index.entries().len(), 1);
        let font = index.get("fonts/body.ttf").unwrap();
        let mut decoded = vec![0; font.length as usize];
        font.read_range(
            &mut archive,
            0,
            &mut decoded,
            &mut ReadScratch::new(),
            || false,
        )
        .unwrap();
        assert_eq!(decoded, bytes, "font collections retain all input bytes");
        assert!(
            !output.join("fonts/body.ttf").exists(),
            "the VPK carries one font payload"
        );
        assert_eq!(
            fs::read(output.join("licenses/launcher-font.txt")).unwrap(),
            b"font redistribution license"
        );
    }
}
