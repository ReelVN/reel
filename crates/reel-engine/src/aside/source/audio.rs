use aside_compiler::{
    Diagnostic, DiagnosticCode, Severity,
    editor::EditorModule,
    hir::{Form, Hir, NodeId},
    lowering::BranchSelection,
    strings::static_text,
};

/// Report statically known misuse at the argument, including through imports.
pub fn audio_diagnostics(module: EditorModule<'_>) -> impl Iterator<Item = Diagnostic> {
    check_selected(module, None)
}

pub(super) fn check_selected<'a>(
    module: EditorModule<'a>,
    selected: Option<&'a BranchSelection>,
) -> impl Iterator<Item = Diagnostic> + 'a {
    let contract = super::contracts()
        .iter()
        .find(|contract| contract.symbol == "reel::audio::play")
        .expect("audio.play is registered");
    let hir = module.hir();
    hir.nodes
        .iter()
        .enumerate()
        .filter_map(move |(index, node)| {
            let Form::Call {
                callee,
                arguments: Some(arguments),
                ..
            } = node.form
            else {
                return None;
            };
            let reference = module.canonical_reference(callee)?;
            if reference.module != "reel::audio"
                || reference.export.as_deref() != Some("play")
                || selected.is_some_and(|selected| !selected.contains(NodeId(index as u32)))
            {
                return None;
            }
            check_play_arguments(hir, arguments, &contract.parameters)
        })
}

fn check_play_arguments(
    hir: &Hir,
    arguments: NodeId,
    parameters: &[aside_host::NativeParameter],
) -> Option<Diagnostic> {
    let mut positional = parameters.iter();
    let mut group = None;
    let mut skip = None;
    for argument in hir.children(arguments) {
        let Form::Argument { name, value } = hir.node(argument).form else {
            return None;
        };
        let name = if let Some(name) = name {
            hir.name(name.name?)
        } else {
            positional.next()?.name.as_str()
        };
        match name {
            "group" => group = Some(value),
            "skip" => skip = Some(value),
            _ => (),
        }
    }
    let skip = skip?;
    if static_text(hir, skip)?.as_str() != "omit" {
        return None;
    }
    if let Some(group) = group
        && static_text(hir, group)?.as_str() == "voice"
    {
        return None;
    }
    Some(Diagnostic {
        code: DiagnosticCode::InvalidCall,
        severity: Severity::Error,
        range: hir.node(group.unwrap_or(skip)).span,
        message: "audio.play with skip: \"omit\" requires group: \"voice\"".into(),
        related: Vec::new(),
        fix: None,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn literal_audio_omission_points_to_wrong_group() {
        for (arguments, invalid) in [
            (r#"skip: "omit""#, r#""omit""#),
            (r#"group: "music", skip: "omit""#, r#""music""#),
            (r#"group: "voice", skip: "omit""#, ""),
            (r#"group: group(), skip: "omit""#, ""),
        ] {
            let source = format!(
                r#"fn group() {{ "voice" }} fn main() {{ audio.play("a.ogg", {arguments}); }}"#
            );
            let result = super::super::compile_module(&source, "game::main", "src/main.aside", 1);
            if invalid.is_empty() {
                assert!(result.is_ok(), "{:?}", result.err());
            } else {
                let Some(super::super::SourceError::NativeArgument(error)) = result.err() else {
                    panic!("expected native argument diagnostic");
                };
                let (start, end) = (error.diagnostic.range.start(), error.diagnostic.range.end());
                assert_eq!(
                    &source[u32::from(start) as usize..u32::from(end) as usize],
                    invalid
                );
            }
        }
    }
}
