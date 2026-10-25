//! Assemble the native modules, launcher font and generated LiveArea artwork
//! consumed by `reel-player`'s `package.metadata.vita.assets` directory.
//! `cargo +nightly vita build vpk` owns executable conversion and VPK packaging.

use anyhow::{Context, Result, bail, ensure};
use reel_package::archive::{ArchiveKind, ArchiveWriter};
use std::{fs, io::BufWriter, path::Path};

const MODULES: [&str; 4] = [
    "libgpu_es4_ext.suprx",
    "libpvrPSP2_WSEGL.suprx",
    "libIMGEGL.suprx",
    "libGLESv2.suprx",
];

const LIVEAREA: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<livearea style="a1" format-ver="01.00" content-rev="1">
    <livearea-background><image>bg0.png</image></livearea-background>
    <gate><startup-image>startup.png</startup-image></gate>
</livearea>
"#;

/// Prepare `target/vita/assets` for the launcher VPK on the current build host.
///
/// Arguments are `--modules DIR --font FILE --font-license FILE`. The module
/// directory must contain all four PVR GLES modules. The supplied OpenType or
/// TrueType font (including a TTC) retains its complete logical bytes at
/// `fonts/body.ttf` inside `launcher.rpk`; its content digest is computed during
/// packaging so startup mounts only the archive index before loading the font.
/// The font's redistribution license is copied to `licenses/launcher-font.txt`.
/// Generated indexed PNG artwork uses the Vita LiveArea image dimensions.
pub fn assets(args: &[String]) -> Result<()> {
    let mut modules = None;
    let mut font = None;
    let mut license = None;
    for pair in args.chunks(2) {
        let [option, value] = pair else {
            bail!("vita-assets expects --modules DIR --font FILE --font-license FILE");
        };
        let slot = match option.as_str() {
            "--modules" => &mut modules,
            "--font" => &mut font,
            "--font-license" => &mut license,
            _ => bail!("unknown vita-assets option: {option}"),
        };
        ensure!(slot.is_none(), "duplicate vita-assets option: {option}");
        *slot = Some(Path::new(value));
    }
    let modules = modules.context("vita-assets requires --modules DIR")?;
    let font = font.context("vita-assets requires --font FILE")?;
    let license = license.context("vita-assets requires --font-license FILE")?;
    for path in MODULES
        .iter()
        .map(|name| modules.join(name))
        .chain([font.to_owned(), license.to_owned()])
    {
        ensure!(
            fs::metadata(&path)
                .with_context(|| format!("read Vita asset {}", path.display()))?
                .len()
                > 0,
            "empty Vita asset: {}",
            path.display()
        );
    }

    let parent = crate::root().join("target/vita");
    fs::create_dir_all(&parent)?;
    let output = parent.join("assets");
    let staging = parent.join(format!("assets-{}", std::process::id()));
    fs::create_dir(&staging)?;
    let result = assemble(&staging, modules, font, license).and_then(|()| {
        if output.exists() {
            fs::remove_dir_all(&output)?;
        }
        fs::rename(&staging, &output)?;
        Ok(())
    });
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result?;
    println!("Vita launcher assets: {}", output.display());
    Ok(())
}

fn assemble(output: &Path, modules: &Path, font: &Path, license: &Path) -> Result<()> {
    for name in ["module", "licenses", "sce_sys/livearea/contents"] {
        fs::create_dir_all(output.join(name))?;
    }
    for name in MODULES {
        fs::copy(modules.join(name), output.join("module").join(name))?;
    }
    let mut archive = ArchiveWriter::new(
        BufWriter::new(fs::File::create(output.join("launcher.rpk"))?),
        ArchiveKind::Resources,
    )?;
    archive.add("fonts/body.ttf", &mut fs::File::open(font)?)?;
    archive.finish()?;
    fs::copy(license, output.join("licenses/launcher-font.txt"))?;
    fs::write(
        output.join("sce_sys/livearea/contents/template.xml"),
        LIVEAREA,
    )?;
    for (path, width, height, mark) in [
        ("sce_sys/icon0.png", 128, 128, 108.),
        ("sce_sys/pic0.png", 960, 544, 240.),
        ("sce_sys/livearea/contents/bg0.png", 840, 500, 220.),
        ("sce_sys/livearea/contents/startup.png", 280, 158, 126.),
    ] {
        artwork(&output.join(path), width, height, mark)?;
    }
    Ok(())
}

fn artwork(path: &Path, width: u32, height: u32, mark: f32) -> Result<()> {
    // Flat geometric reel artwork needs only four colors. Keeping the source
    // here makes all platform images reproducible without tracked binaries.
    let palette = [16, 24, 42, 22, 35, 56, 78, 207, 184, 235, 244, 249];
    let mut pixels = vec![0; (width * height) as usize];
    for y in 0..height {
        for x in 0..width {
            let px = (x as f32 + 0.5 - width as f32 * 0.5) / mark;
            let py = (y as f32 + 0.5 - height as f32 * 0.5) / mark;
            let radius = px * px + py * py;
            let mut color = u8::from((x + y) % 64 < 2);
            if radius < 0.25 {
                color = 2;
                if radius < 0.0036
                    || [(0.22, 0.), (-0.22, 0.), (0., 0.22), (0., -0.22)]
                        .into_iter()
                        .any(|(cx, cy)| (px - cx).powi(2) + (py - cy).powi(2) < 0.0144)
                {
                    color = 0;
                }
            }
            if px > 0.30 && px < 0.56 && py > 0.28 && py < 0.40 {
                color = 3;
            }
            pixels[(y * width + x) as usize] = color;
        }
    }
    let mut encoder = png::Encoder::new(BufWriter::new(fs::File::create(path)?), width, height);
    encoder.set_color(png::ColorType::Indexed);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_palette(&palette[..]);
    encoder.write_header()?.write_image_data(&pixels)?;
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
        for name in MODULES {
            fs::write(source.join(name), name.as_bytes()).unwrap();
        }
        let font = source.join("font.ttc");
        let license = source.join("license.txt");
        let bytes = (0..150_000)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        fs::write(&font, &bytes).unwrap();
        fs::write(&license, b"font redistribution license").unwrap();
        assemble(&output, &source, &font, &license).unwrap();
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
        for name in MODULES {
            assert_eq!(
                fs::read(output.join("module").join(name)).unwrap(),
                name.as_bytes()
            );
        }
    }
}
