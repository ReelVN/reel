//! Compose parsed text without reparsing its immutable prefixes.
use super::*;
use std::collections::TryReserveError;

fn append_source(output: &mut String, text: &Text) -> Result<(), TryReserveError> {
    if let Some(markup) = text.markup() {
        output.try_reserve(markup.len())?;
        output.push_str(markup);
    } else {
        let escapes = text.bytes().filter(|&byte| byte == b'[').count();
        output.try_reserve(text.len().saturating_add(escapes))?;
        for part in text.split_inclusive('[') {
            output.push_str(part);
            if part.ends_with('[') {
                output.push('[');
            }
        }
    }
    Ok(())
}

fn joined_vec<T: Clone>(a: &[T], b: &[T]) -> Result<Vec<T>, TryReserveError> {
    let mut values = Vec::new();
    values.try_reserve_exact(a.len().saturating_add(b.len()))?;
    values.extend_from_slice(a);
    values.extend_from_slice(b);
    Ok(values)
}

impl Text {
    /// Join complete text fragments, retaining their parsed style and timing.
    /// Plain fragments remain literal even when the other fragment has markup.
    /// Styles close at each fragment boundary; an adjacent equal span is merged.
    pub fn concat(&self, suffix: &Self) -> Result<Self, TryReserveError> {
        let mut plain = String::new();
        plain.try_reserve_exact(self.len().saturating_add(suffix.len()))?;
        plain.push_str(self);
        plain.push_str(suffix);
        if self.rich.is_none() && suffix.rich.is_none() {
            return Ok(plain.into());
        }
        let mut markup = String::new();
        append_source(&mut markup, self)?;
        append_source(&mut markup, suffix)?;
        let offset = self.len();
        let mut spans = joined_vec(self.spans(), suffix.spans())?;
        for span in &mut spans[self.spans().len()..] {
            span.range.start += offset;
            span.range.end += offset;
        }
        let boundary = self.spans().len();
        if boundary != 0 && boundary < spans.len() {
            let previous = &spans[boundary - 1];
            let next = &spans[boundary];
            if previous.range.end == next.range.start && previous.style == next.style {
                spans[boundary - 1].range.end = next.range.end;
                spans.remove(boundary);
            }
        }
        let mut ruby = joined_vec(self.ruby(), suffix.ruby())?;
        for value in &mut ruby[self.ruby().len()..] {
            value.range.start += offset;
            value.range.end += offset;
        }
        let mut links = joined_vec(self.links(), suffix.links())?;
        for value in &mut links[self.links().len()..] {
            value.range.start += offset;
            value.range.end += offset;
        }
        let mut cues = joined_vec(self.cues(), suffix.cues())?;
        for cue in &mut cues[self.cues().len()..] {
            cue.offset += offset;
        }
        // Both fragments retain the same font Arcs as their parsed input.
        let mut fonts = Vec::new();
        fonts.try_reserve_exact(
            spans
                .iter()
                .filter(|span| span.style.font.is_some())
                .count(),
        )?;
        for font in spans.iter().filter_map(|span| span.style.font.as_ref()) {
            fonts.push((Arc::as_ptr(font) as *const () as usize, font.len()));
        }
        fonts.sort_unstable_by_key(|&(identity, _)| identity);
        fonts.dedup_by_key(|entry| entry.0);
        let font_bytes = fonts
            .iter()
            .map(|&(_, len)| len + 2 * std::mem::size_of::<usize>())
            .sum();
        Ok(Self {
            plain: plain.into(),
            rich: Some(Arc::new(RichText {
                markup: markup.into(),
                spans,
                ruby,
                links,
                cues,
                continue_after: self.continue_after() || suffix.continue_after(),
                font_bytes,
            })),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parsed_composition_matches_markup_and_preserves_shared_annotations() {
        let parts = [
            Text::from("literal [b]中文"),
            Text::parse("[font=Example][b]A[/b][/font][ruby=ほし]星[/ruby]").unwrap(),
            Text::parse("[ruby=よる]夜[/ruby][speed=2x][pause=0.2][link=a]B[/link][/speed][next]")
                .unwrap(),
            Text::parse("[b]C[/b]").unwrap(),
            Text::parse("[b]D[/b]").unwrap(),
            Text::from("[[["),
        ];
        let mut combined = Text::default();
        for part in &parts {
            combined = combined.concat(part).unwrap();
            if let Some(source) = combined.markup() {
                let reparsed = Text::parse(source.clone()).unwrap();
                assert_eq!(combined.as_ref(), reparsed.as_ref());
                assert_eq!(combined.spans(), reparsed.spans());
                assert_eq!(combined.ruby(), reparsed.ruby());
                assert_eq!(combined.links(), reparsed.links());
                assert_eq!(combined.cues(), reparsed.cues());
                assert_eq!(combined.continue_after(), reparsed.continue_after());
            }
        }
        assert!(Arc::ptr_eq(
            &parts[1].ruby()[0].annotation,
            &combined.ruby()[0].annotation
        ));
        assert!(Arc::ptr_eq(
            &parts[2].links()[0].id,
            &combined.links()[0].id
        ));
        let fragment = Text::parse("[font=Shared]A[/font] ").unwrap();
        let shared = fragment.concat(&fragment).unwrap();
        let restored = Text::parse(shared.markup().unwrap().clone()).unwrap();
        assert_eq!(shared, restored);
        assert!(shared.storage_bytes() < restored.storage_bytes());
    }
}
