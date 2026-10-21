//! Position encoding is confined to the protocol boundary; source stays UTF-8.
use lsp_types::{Position, PositionEncodingKind, Range};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Encoding { Utf8, Utf16 }

impl Encoding {
    pub fn negotiate(encodings: Option<&[PositionEncodingKind]>) -> Self {
        if encodings.is_some_and(|values| values.contains(&PositionEncodingKind::UTF8)) {Self::Utf8} else {Self::Utf16}
    }
    pub fn kind(self) -> PositionEncodingKind {
        match self {Self::Utf8=>PositionEncodingKind::UTF8,Self::Utf16=>PositionEncodingKind::UTF16}
    }
}

#[derive(Debug,Clone)]
pub(crate) struct LineIndex { starts:Vec<usize> }
impl LineIndex {
    pub fn new(source:&str)->Self {
        Self{starts:std::iter::once(0).chain(source.match_indices('\n').map(|(i,_)|i+1)).collect()}
    }
    pub fn offset(&self,source:&str,position:Position,encoding:Encoding)->Option<usize> {
        let start=*self.starts.get(position.line as usize)?;
        let end=self.starts.get(position.line as usize+1).copied().unwrap_or(source.len());
        let line=source[start..end].trim_end_matches(['\r','\n']);
        let column=position.character as usize;
        match encoding {
            Encoding::Utf8=>{
                let column=column.min(line.len());
                line.is_char_boundary(column).then_some(start+column)
            }
            Encoding::Utf16=>{
                let mut units=0;
                for (offset,ch) in line.char_indices() {
                    if units==column {return Some(start+offset);}
                    units+=ch.len_utf16();
                    if units>column {return None;}
                }
                Some(start+line.len())
            }
        }
    }
    pub fn position(&self,source:&str,offset:usize,encoding:Encoding)->Option<Position> {
        if offset>source.len() || !source.is_char_boundary(offset) {return None;}
        let line=self.starts.partition_point(|&start|start<=offset)-1;
        let end=self.starts.get(line+1).copied().unwrap_or(source.len());
        let visible=source[self.starts[line]..end].trim_end_matches(['\r','\n']);
        let prefix=&source[self.starts[line]..offset.min(self.starts[line]+visible.len())];
        let column=match encoding {Encoding::Utf8=>prefix.len(),Encoding::Utf16=>prefix.encode_utf16().count()};
        Some(Position::new(line as u32,column as u32))
    }
    pub fn range(&self,source:&str,span:aside_compiler::TextRange,encoding:Encoding)->Range {
        Range{start:self.position(source,u32::from(span.start()) as usize,encoding).expect("compiler span starts at a valid UTF-8 source boundary"),
            end:self.position(source,u32::from(span.end()) as usize,encoding).expect("compiler span ends at a valid UTF-8 source boundary")}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chinese_emoji_crlf_and_encoding_negotiation() {
        let source="中文😀x\r\n下一行";let index=LineIndex::new(source);
        assert_eq!(Encoding::negotiate(None),Encoding::Utf16);
        assert_eq!(Encoding::negotiate(Some(&[PositionEncodingKind::UTF16,PositionEncodingKind::UTF8])),Encoding::Utf8);
        for encoding in [Encoding::Utf8,Encoding::Utf16] {
            for offset in [0,3,6,10,11,13,16,19,22] {
                let position=index.position(source,offset,encoding).unwrap();
                assert_eq!(index.offset(source,position,encoding),Some(offset));
            }
        }
        assert_eq!(index.offset(source,Position::new(0,3),Encoding::Utf16),None);
        assert_eq!(index.offset(source,Position::new(0,7),Encoding::Utf8),None);
        assert_eq!(index.offset(source,Position::new(0,99),Encoding::Utf16),Some(11));
        assert_eq!(index.offset(source,Position::new(0,99),Encoding::Utf8),Some(11));
        assert_eq!(index.position(source,12,Encoding::Utf16),Some(Position::new(0,5)));
    }
}
