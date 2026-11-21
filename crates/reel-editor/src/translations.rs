//! Present the engine's translation analysis without parsing or evaluating a second language.
use aside_compiler::{
    Diagnostic, DiagnosticCode, Severity, TextRange, TextSize,
    editor::EditorModule,
    hir::{FieldKey, Form, HirKind},
};
use aside_lsp::{SourceCompletions, SourceFeatures, SourceHover};
use lsp_types::{CompletionItem, CompletionItemKind};
use reel_engine::aside::source::{TranslationSite, analyze_translations};
use std::sync::Arc;

pub(super) fn analyze(module: EditorModule<'_>) -> SourceFeatures {
    let analysis = analyze_translations(module);
    let mut features = SourceFeatures {
        diagnostics: analysis
            .diagnostics
            .into_iter()
            .map(|diagnostic| Diagnostic {
                code: DiagnosticCode::InvalidCall,
                severity: Severity::Error,
                range: diagnostic.span,
                message: diagnostic.message,
                related: Vec::new(),
                fix: None,
            })
            .collect(),
        ..Default::default()
    };
    for site in analysis.calls {
        let mut markdown = String::new();
        if let Some(source) = &site.source {
            markdown.push_str("**Source**\n\n");
            code_block(&mut markdown, source);
        }
        for variant in &site.variants {
            if let Some(text) = &variant.text {
                markdown.push_str("\n\n**");
                markdown.push_str(&variant.locale.replace(['*', '_', '`', '[', ']'], ""));
                markdown.push_str("**\n\n");
                code_block(&mut markdown, text);
            }
        }
        let markdown: Arc<str> = markdown.into();
        for span in site
            .source_span
            .into_iter()
            .chain(site.variants.iter().map(|v| v.span))
        {
            let mut cursor = span.start();
            for hole in site
                .holes
                .iter()
                .filter(|hole| span.contains_range(hole.expression))
            {
                if cursor < hole.expression.start() {
                    features.hovers.push(SourceHover {
                        range: TextRange::new(cursor, hole.expression.start()),
                        markdown: markdown.clone(),
                    });
                }
                cursor = hole.expression.end();
            }
            if cursor < span.end() {
                features.hovers.push(SourceHover {
                    range: TextRange::new(cursor, span.end()),
                    markdown: markdown.clone(),
                });
            }
        }
        completion_regions(module, &site, &mut features.completions);
    }
    features
}

fn code_block(markdown: &mut String, text: &str) {
    for (index, line) in text.split('\n').enumerate() {
        if index != 0 {
            markdown.push('\n');
        }
        markdown.push_str("    ");
        markdown.push_str(line);
    }
}

fn completion_regions(
    module: EditorModule<'_>,
    site: &TranslationSite,
    output: &mut Vec<SourceCompletions>,
) {
    let Some(table_span) = site.table_span else {
        return;
    };
    let hir = module.hir();
    let Some((index, table)) = hir
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| node.kind == HirKind::Object && table_span.contains_range(node.span))
    else {
        return;
    };
    let items: Arc<[CompletionItem]> = [
        "en", "zh-CN", "zh-TW", "ja", "ko", "fr", "de", "es", "pt-BR", "ru",
    ]
    .into_iter()
    .filter(|locale| !site.locales.iter().any(|present| present == locale))
    .map(|locale| {
        let key = if locale.contains('-') {
            format!("\"{locale}\"")
        } else {
            locale.to_owned()
        };
        let original = site
            .source_span
            .map(|span| hir.text(span))
            .unwrap_or("\"\"");
        CompletionItem {
            label: locale.into(),
            kind: Some(CompletionItemKind::FIELD),
            detail: Some("Inline translation".into()),
            insert_text: Some(format!("{key}: {original}")),
            ..Default::default()
        }
    })
    .collect();
    let mut cursor = table.span.start() + TextSize::from(1);
    for field in hir.children(aside_compiler::hir::NodeId(index as u32)) {
        let node = hir.node(field);
        let key_end = match node.form {
            Form::Field {
                key: FieldKey::Name(name),
                ..
            } => name.span.end(),
            Form::Field {
                key: FieldKey::Expression(key),
                ..
            } => hir.node(key).span.end(),
            _ => node.span.start(),
        };
        if cursor <= key_end {
            output.push(SourceCompletions {
                range: TextRange::new(cursor, key_end),
                items: items.clone(),
            });
        }
        cursor = node.span.end();
    }
    let end = table.span.end() - TextSize::from(1);
    if cursor <= end {
        output.push(SourceCompletions {
            range: TextRange::new(cursor, end),
            items,
        });
    }
}
