//! Immutable author text, shared by narrative services and paragraph layout.
use std::{ops::Range, sync::Arc};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub emphasis: bool,
    pub size: Option<f32>,
    pub color: Option<[u8; 4]>,
    pub link: Option<Arc<str>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextSpan {
    pub range: Range<usize>,
    pub style: TextStyle,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ruby {
    pub range: Range<usize>,
    pub annotation: Arc<str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextLink {
    pub range: Range<usize>,
    pub id: Arc<str>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TextSpeed {
    PerSecond(f64),
    PlayerMultiple(f64),
}
impl Default for TextSpeed {
    fn default() -> Self {
        Self::PlayerMultiple(1.)
    }
}
impl TextSpeed {
    pub fn per_second(self, player_speed: f64) -> f64 {
        match self {
            Self::PerSecond(speed) => speed,
            Self::PlayerMultiple(factor) => (player_speed * factor).min(f64::MAX),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TextCueKind {
    Speed(TextSpeed),
    Pause(std::time::Duration),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextCue {
    /// UTF-8 boundary in the body; playback rounds forward to a whole grapheme.
    pub offset: usize,
    pub kind: TextCueKind,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Text {
    plain: Arc<str>,
    rich: Option<Arc<RichText>>,
}

#[derive(Debug, PartialEq)]
struct RichText {
    markup: Arc<str>,
    spans: Vec<TextSpan>,
    ruby: Vec<Ruby>,
    links: Vec<TextLink>,
    cues: Vec<TextCue>,
    continue_after: bool,
}

impl Default for Text {
    fn default() -> Self {
        Self::from("")
    }
}
impl From<&str> for Text {
    fn from(text: &str) -> Self {
        Self::from(Arc::<str>::from(text))
    }
}
impl From<String> for Text {
    fn from(text: String) -> Self {
        Self::from(Arc::<str>::from(text))
    }
}
impl From<Arc<str>> for Text {
    fn from(plain: Arc<str>) -> Self {
        Self { plain, rich: None }
    }
}
impl std::ops::Deref for Text {
    type Target = str;
    fn deref(&self) -> &str {
        &self.plain
    }
}
impl AsRef<str> for Text {
    fn as_ref(&self) -> &str {
        &self.plain
    }
}
impl std::fmt::Display for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.plain.fmt(f)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkupError {
    pub offset: usize,
    pub message: &'static str,
}
impl std::fmt::Display for MarkupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rich text byte {}: {}", self.offset, self.message)
    }
}
impl std::error::Error for MarkupError {}

struct OpenTag<'a> {
    name: &'a str,
    offset: usize,
    start: usize,
    style: TextStyle,
    ruby: Option<&'a str>,
    speed: TextSpeed,
}

impl Text {
    pub fn plain(&self) -> &Arc<str> {
        &self.plain
    }
    pub fn markup(&self) -> Option<&Arc<str>> {
        self.rich.as_ref().map(|r| &r.markup)
    }
    pub fn spans(&self) -> &[TextSpan] {
        self.rich.as_ref().map_or(&[], |r| &r.spans)
    }
    pub fn ruby(&self) -> &[Ruby] {
        self.rich.as_ref().map_or(&[], |r| &r.ruby)
    }
    pub fn links(&self) -> &[TextLink] {
        self.rich.as_ref().map_or(&[], |r| &r.links)
    }
    pub fn has_links(&self) -> bool {
        !self.links().is_empty()
    }
    pub fn cues(&self) -> &[TextCue] {
        self.rich.as_ref().map_or(&[], |r| &r.cues)
    }
    pub fn continue_after(&self) -> bool {
        self.rich.as_ref().is_some_and(|r| r.continue_after)
    }
    pub fn storage_bytes(&self) -> usize {
        self.plain.len()
            + self.rich.as_ref().map_or(0, |r| {
                std::mem::size_of::<RichText>()
                    + r.markup.len()
                    + r.spans.capacity() * std::mem::size_of::<TextSpan>()
                    + r.ruby.capacity() * std::mem::size_of::<Ruby>()
                    + r.links.capacity() * std::mem::size_of::<TextLink>()
                    + r.cues.capacity() * std::mem::size_of::<TextCue>()
                    + r.ruby.iter().map(|r| r.annotation.len()).sum::<usize>()
                    + r.links.iter().map(|link| link.id.len()).sum::<usize>()
            })
    }
    pub fn style_at(&self, offset: usize) -> Option<&TextStyle> {
        let index = self
            .spans()
            .partition_point(|span| span.range.end <= offset);
        self.spans()
            .get(index)
            .filter(|s| s.range.contains(&offset))
            .map(|s| &s.style)
    }
    pub fn parse(markup: impl Into<Arc<str>>) -> Result<Self, MarkupError> {
        let markup = markup.into();
        let mut plain = String::with_capacity(markup.len());
        let mut spans: Vec<TextSpan> = Vec::new();
        let mut ruby = Vec::new();
        let mut links = Vec::new();
        let mut cues = Vec::new();
        let mut continue_after = false;
        let mut speed = TextSpeed::default();
        let mut tags: Vec<OpenTag<'_>> = Vec::new();
        let mut style = TextStyle::default();
        let mut cursor = 0;
        while cursor < markup.len() {
            let rest = &markup[cursor..];
            if rest.starts_with('[') && !rest.starts_with("[[") {
                let end = rest.find(']').ok_or(MarkupError {
                    offset: cursor,
                    message: "unclosed tag",
                })?;
                let tag = &rest[1..end];
                let error = |message| MarkupError {
                    offset: cursor,
                    message,
                };
                if let Some(name) = tag.strip_prefix('/') {
                    let open = tags
                        .pop()
                        .ok_or_else(|| error("closing tag without opener"))?;
                    if open.name != name {
                        return Err(error("mismatched closing tag"));
                    }
                    if let Some(annotation) = open.ruby {
                        if open.start == plain.len() {
                            return Err(error("ruby requires base text"));
                        }
                        ruby.push(Ruby {
                            range: open.start..plain.len(),
                            annotation: annotation.into(),
                        });
                    }
                    if name == "link" && open.start < plain.len() {
                        links.push(TextLink {
                            range: open.start..plain.len(),
                            id: style.link.clone().unwrap(),
                        });
                    }
                    if name == "speed" {
                        speed = open.speed;
                        cues.push(TextCue {
                            offset: plain.len(),
                            kind: TextCueKind::Speed(speed),
                        });
                    }
                    style = open.style;
                } else {
                    let (name, argument) = tag
                        .split_once('=')
                        .map_or((tag, None), |(a, b)| (a, Some(b)));
                    let mut next = style.clone();
                    let mut annotation = None;
                    let previous_speed = speed;
                    match (name, argument) {
                        ("next", None) => {
                            continue_after = true;
                            cursor += end + 1;
                            continue;
                        }
                        ("pause", Some(value)) => {
                            let seconds: f64 =
                                value.parse().map_err(|_| error("invalid pause duration"))?;
                            let duration = std::time::Duration::try_from_secs_f64(seconds)
                                .map_err(|_| {
                                    error("pause duration must be finite and nonnegative")
                                })?;
                            cues.push(TextCue {
                                offset: plain.len(),
                                kind: TextCueKind::Pause(duration),
                            });
                            cursor += end + 1;
                            continue;
                        }
                        ("speed", Some(value)) => {
                            let (value, multiple) = value
                                .strip_suffix('x')
                                .map_or((value, false), |v| (v, true));
                            let value: f64 =
                                value.parse().map_err(|_| error("invalid text speed"))?;
                            if !value.is_finite() || value < 0. {
                                return Err(error("text speed must be finite and nonnegative"));
                            }
                            speed = if multiple {
                                TextSpeed::PlayerMultiple(value)
                            } else {
                                TextSpeed::PerSecond(value)
                            };
                            cues.push(TextCue {
                                offset: plain.len(),
                                kind: TextCueKind::Speed(speed),
                            });
                        }
                        ("b", None) => next.bold = true,
                        ("i", None) => next.italic = true,
                        ("u", None) => next.underline = true,
                        ("s", None) => next.strike = true,
                        ("em", None) => next.emphasis = true,
                        ("size", Some(value)) => {
                            let size: f32 =
                                value.parse().map_err(|_| error("invalid font size"))?;
                            if !size.is_finite() || size <= 0. {
                                return Err(error("font size must be positive and finite"));
                            }
                            next.size = Some(size);
                        }
                        ("color", Some(value)) => {
                            next.color = Some(
                                parse_color(value)
                                    .ok_or_else(|| error("expected #RRGGBB or #RRGGBBAA color"))?,
                            );
                        }
                        ("link", Some(value)) if !value.is_empty() => {
                            if style.link.is_some() {
                                return Err(error("links cannot contain another link"));
                            }
                            next.link = Some(value.into());
                        }
                        ("ruby", Some(value)) if !value.is_empty() => {
                            if tags.iter().any(|tag| tag.ruby.is_some()) {
                                return Err(error("ruby base cannot contain another ruby"));
                            }
                            annotation = Some(value);
                        }
                        _ => return Err(error("unknown tag or invalid tag argument")),
                    }
                    tags.push(OpenTag {
                        name,
                        offset: cursor,
                        start: plain.len(),
                        style,
                        ruby: annotation,
                        speed: previous_speed,
                    });
                    style = next;
                }
                cursor += end + 1;
            } else {
                let (text, consumed) = if rest.starts_with("[[") {
                    ("[", 2)
                } else {
                    let end = rest.find('[').unwrap_or(rest.len());
                    (&rest[..end], end)
                };
                let start = plain.len();
                plain.push_str(text);
                if style != TextStyle::default() {
                    if let Some(last) = spans.last_mut()
                        && last.range.end == start
                        && last.style == style
                    {
                        last.range.end = plain.len();
                    } else {
                        spans.push(TextSpan {
                            range: start..plain.len(),
                            style: style.clone(),
                        });
                    }
                }
                cursor += consumed;
            }
        }
        if let Some(tag) = tags.last() {
            return Err(MarkupError {
                offset: tag.offset,
                message: "unclosed tag",
            });
        }
        Ok(Self {
            plain: plain.into(),
            rich: Some(Arc::new(RichText {
                markup,
                spans,
                ruby,
                links,
                cues,
                continue_after,
            })),
        })
    }
}

fn parse_color(value: &str) -> Option<[u8; 4]> {
    let value = value.strip_prefix('#')?;
    if !matches!(value.len(), 6 | 8) || !value.is_ascii() {
        return None;
    }
    let mut color = [255; 4];
    for (i, part) in value.as_bytes().chunks_exact(2).enumerate() {
        color[i] = u8::from_str_radix(std::str::from_utf8(part).ok()?, 16).ok()?;
    }
    Some(color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_markup_keeps_utf8_ranges_styles_and_plain_identity() {
        let t = Text::parse("[[b] [b]粗[color=#12ab34cc]色[/color]粗[/b] [link=star][ruby=ほし]星[/ruby][/link][em]空[/em]").unwrap();
        assert_eq!(&*t, "[b] 粗色粗 星空");
        let color = t.find('色').unwrap();
        assert!(t.style_at(color).unwrap().bold);
        assert_eq!(t.style_at(color).unwrap().color, Some([18, 171, 52, 204]));
        assert_eq!(t.style_at(color + 3).unwrap().color, None);
        assert_eq!(&t[t.ruby()[0].range.clone()], "星");
        assert_eq!(t.ruby()[0].range, t.links()[0].range);
        assert_eq!(&*t.links()[0].id, "star");
        let clone = t.clone();
        assert!(Arc::ptr_eq(t.plain(), clone.plain()));
        assert!(Arc::ptr_eq(
            t.rich.as_ref().unwrap(),
            clone.rich.as_ref().unwrap()
        ));
        assert_eq!(&*Text::from("[b]literal[/b]"), "[b]literal[/b]");
    }

    #[test]
    fn malformed_markup_reports_the_offending_byte_without_silent_repair() {
        for (s, offset) in [
            ("星[b]x[/i]", 7),
            ("星[b]x", 3),
            ("[size=NaN]x[/size]", 0),
            ("[color=#123]x[/color]", 0),
            ("[ruby=a][/ruby]", 8),
            ("[link=a][link=b]x[/link][/link]", 8),
            ("[ruby=a][ruby=b]x[/ruby][/ruby]", 8),
            ("[speed=-1]x[/speed]", 0),
            ("[speed=NaNx]x[/speed]", 0),
            ("[pause=inf]x", 0),
            ("[pause=-1]x", 0),
        ] {
            assert_eq!(Text::parse(s).unwrap_err().offset, offset, "{s}");
        }
    }
}
