//! Per-image atlas view envelope. The referenced page is a separate Reel texture.
use crate::texture::{ColorEncoding, ImageView};

/// One logical image in a shared prepared texture page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedAtlasView {
    /// Logical resource path of the physical page, resolved by the ordinary asset service.
    pub page: String,
    /// Author dimensions and the page-local content rectangle.
    pub image: ImageView,
    /// Original channel encoding for numeric coverage samples.
    pub encoding: ColorEncoding,
}
impl PreparedAtlasView {
    /// Fixed signature for the view envelope, independent of the author path.
    pub const MAGIC: &'static [u8; 8] = b"REELATL1";
    /// Validate the view and encode it as fixed little-endian metadata plus UTF-8 path.
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        self.validate()?;
        let length = u32::try_from(self.page.len()).map_err(|_| "atlas page path too long")?;
        let mut bytes = Vec::with_capacity(40 + self.page.len());
        bytes.extend_from_slice(Self::MAGIC);
        for value in self
            .image
            .logical_size
            .into_iter()
            .chain(self.image.content_origin)
            .chain(self.image.content_size)
            .chain([
                match self.encoding {
                    ColorEncoding::Srgb => 0,
                    ColorEncoding::Linear => 1,
                },
                length,
            ])
        {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(self.page.as_bytes());
        Ok(bytes)
    }
    /// Parse the complete envelope; page bounds are checked when the page resolves.
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() < 40 || &bytes[..8] != Self::MAGIC {
            return Err("invalid atlas view header");
        }
        let word = |i: usize| u32::from_le_bytes(bytes[8 + i * 4..12 + i * 4].try_into().unwrap());
        let length = word(7) as usize;
        if bytes.len().checked_sub(40) != Some(length) {
            return Err("invalid atlas view path length");
        }
        let encoding = match word(6) {
            0 => ColorEncoding::Srgb,
            1 => ColorEncoding::Linear,
            _ => return Err("invalid atlas view encoding"),
        };
        let result = Self {
            page: std::str::from_utf8(&bytes[40..])
                .map_err(|_| "invalid atlas page path")?
                .to_owned(),
            image: ImageView {
                logical_size: [word(0), word(1)],
                content_origin: [word(2), word(3)],
                content_size: [word(4), word(5)],
            },
            encoding,
        };
        result.validate()?;
        Ok(result)
    }
    fn validate(&self) -> Result<(), &'static str> {
        if self.page.is_empty()
            || self.page.contains(['\\', ':', '\0'])
            || self
                .page
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err("invalid atlas page path");
        }
        if self.image.logical_size.contains(&0)
            || self.image.content_size.contains(&0)
            || (0..2).any(|i| {
                self.image.content_origin[i]
                    .checked_add(self.image.content_size[i])
                    .is_none()
            })
        {
            return Err("invalid atlas view dimensions");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn view_roundtrip_and_page_bounds() {
        let view = PreparedAtlasView {
            page: "__reel/atlas/key/page.reel-texture".into(),
            image: ImageView {
                logical_size: [31, 17],
                content_origin: [8, 12],
                content_size: [16, 9],
            },
            encoding: ColorEncoding::Srgb,
        };
        let data = view.encode().unwrap();
        assert_eq!(PreparedAtlasView::decode(&data).unwrap(), view);
        assert!(view.image.validate([24, 21]).is_ok());
        assert!(view.image.validate([23, 21]).is_err());
        let (scale, offset) = view.image.uv_transform([32, 32]);
        assert_eq!(offset, [0.25, 0.375]);
        assert_eq!(scale, [16.0 / 31.0 / 32.0, 9.0 / 17.0 / 32.0]);
        let mut malformed = data.clone();
        malformed.push(0);
        assert!(PreparedAtlasView::decode(&malformed).is_err());
        assert!(
            PreparedAtlasView {
                page: "../other".into(),
                ..view
            }
            .encode()
            .is_err()
        );
    }
}
