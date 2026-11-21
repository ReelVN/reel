use super::Identity;
use icns::{IconFamily, IconType, PixelFormat};
use image::{DynamicImage, imageops};
use std::{fs, io, path::Path};

pub(super) fn write_bundle(
    app: &Path,
    identity: &Identity,
    icon: Option<DynamicImage>,
) -> io::Result<u64> {
    let short_name: String = identity.title.chars().take(15).collect();
    let icon_entry = if icon.is_some() {
        "    <key>CFBundleIconFile</key>\n    <string>game.icns</string>\n"
    } else {
        ""
    };
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
    <key>CFBundleExecutable</key>
    <string>game</string>
    <key>CFBundleIdentifier</key>
    <string>{}</string>
    <key>CFBundleDisplayName</key>
    <string>{}</string>
    <key>CFBundleName</key>
    <string>{}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
{icon_entry}  </dict>
</plist>
"#,
        identity.id,
        xml_text(&identity.title),
        xml_text(&short_name),
    );
    let contents = app.join("Contents");
    fs::write(contents.join("Info.plist"), &plist)?;
    let mut written_bytes = plist.len() as u64;
    if let Some(icon) = icon {
        let file = fs::File::create(contents.join("Resources/game.icns"))?;
        write_icon(&file, icon)?;
        written_bytes += file.metadata()?.len();
    }
    Ok(written_bytes)
}

fn xml_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn write_icon(file: &fs::File, icon: DynamicImage) -> io::Result<()> {
    let icon = icon.into_rgba8();
    let mut family = IconFamily::new();
    for icon_type in [
        IconType::RGBA32_16x16,
        IconType::RGBA32_16x16_2x,
        IconType::RGBA32_32x32,
        IconType::RGBA32_32x32_2x,
        IconType::RGBA32_128x128,
        IconType::RGBA32_128x128_2x,
        IconType::RGBA32_256x256,
        IconType::RGBA32_256x256_2x,
        IconType::RGBA32_512x512,
        IconType::RGBA32_512x512_2x,
    ] {
        let width = icon_type.pixel_width();
        let height = icon_type.pixel_height();
        let rgba = imageops::resize(&icon, width, height, imageops::FilterType::Lanczos3);
        let image = icns::Image::from_data(PixelFormat::RGBA, width, height, rgba.into_raw())?;
        family.add_icon_with_type(&image, icon_type)?;
    }
    family.write(file)
}
