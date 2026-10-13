//! Generate Aside host declarations using Moxy's Rust syntax toolkit.
#![forbid(unsafe_code)]

use moxy::ast::*;
use moxy::token::{Delim, Group, LitStr, LitVerbatim, Span, Spanner, ToTokenStream, TokenStream};

fn literal(value: impl std::fmt::Display) -> moxy::token::Lit {
    LitVerbatim::new(&value.to_string(), Span::call_site()).into()
}

fn options(tokens: TokenStream, allowed: &[&str]) -> Result<Vec<Meta>, ParseError> {
    let list = List::<Meta, Token![,]>::parse_all(&Parser::from_tokens(&tokens))?;
    let mut result: Vec<Meta> = Vec::new();
    for meta in list {
        if !allowed.iter().any(|name| meta.path.is_ident(name)) {
            return Err(ParseError::new(meta.span(), "unknown Aside binding option"));
        }
        if result
            .iter()
            .any(|old| old.path.as_ident() == meta.path.as_ident())
        {
            return Err(ParseError::new(
                meta.span(),
                "duplicate Aside binding option",
            ));
        }
        result.push(meta);
    }
    Ok(result)
}
fn find<'a>(options: &'a [Meta], name: &str) -> Option<&'a Meta> {
    options.iter().find(|m| m.path.is_ident(name))
}
fn flag(options: &[Meta], name: &str) -> Result<bool, ParseError> {
    match find(options, name) {
        None => Ok(false),
        Some(meta) if matches!(meta.content, MetaContent::Unit) => Ok(true),
        Some(meta) => Err(ParseError::new(meta.span(), "expected a bare flag")),
    }
}
fn expression(meta: &Meta) -> Result<TokenStream, ParseError> {
    if let MetaContent::Expr { expr, .. } = &meta.content {
        Ok(expr.clone())
    } else {
        Err(ParseError::new(meta.span(), "expected an explicit value"))
    }
}
fn helper(attrs: &Attributes, allowed: &[&str]) -> Result<Vec<Meta>, ParseError> {
    let mut matches = attrs.iter().filter(|a| a.path.is_ident("aside"));
    let Some(attr) = matches.next() else {
        return Ok(Vec::new());
    };
    if matches.next().is_some() {
        return Err(ParseError::new(attr.span(), "use one aside attribute"));
    }
    let MetaContent::List(group) = &attr.content else {
        return Err(ParseError::new(attr.span(), "expected #[aside(...)]"));
    };
    options(group.tokens.clone(), allowed)
}
fn constant(meta: &Meta) -> Result<TokenStream, ParseError> {
    let tokens = expression(meta)?;
    if tokens.to_string() == "null" {
        return Ok(moxy::template! { ::aside_host::aside_vm::bytecode::Const::Null });
    }
    let parsed = tokens.clone();
    let expr: Expr = moxy::parse!(parsed)?;
    let literal = matches!(&expr, Expr::Lit(_))
        || matches!(&expr, Expr::Unary(u) if matches!(u.op, UnOp::Neg(_)) && matches!(*u.expr, Expr::Lit(_)));
    if !literal {
        return Err(ParseError::new(
            expr.span(),
            "native defaults must be literal constants",
        ));
    }
    Ok(moxy::template! { ::aside_host::constant({{ tokens }}) })
}
fn type_arguments<'a>(ty: &'a Type, name: &str) -> Option<Vec<&'a Type>> {
    let Type::Path(path) = ty else {
        return None;
    };
    let last = path.path.last()?;
    if last.ident.text() != name {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &last.args else {
        return None;
    };
    Some(
        args.args
            .iter()
            .filter_map(|a| {
                if let GenericArgument::Type(t) = a {
                    Some(t)
                } else {
                    None
                }
            })
            .collect(),
    )
}

/// Export a monomorphic Rust function and generate a matching native binding.
#[proc_macro_attribute]
pub fn native(
    meta: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    // Keep the original compiler group, including distinct opening/closing spans.
    // Converting the opaque body through an AST/token library can lose those spans.
    let mut tokens: Vec<_> = item.into_iter().collect();
    let Some(proc_macro::TokenTree::Group(body)) = tokens.pop() else {
        return ParseError::new(Span::call_site(), "native requires a function body")
            .to_compile_error()
            .into();
    };
    if body.delimiter() != proc_macro::Delimiter::Brace {
        return ParseError::new(body.span().into(), "native requires a function body")
            .to_compile_error()
            .into();
    }
    let signature: proc_macro::TokenStream = tokens.into_iter().collect();
    let mut signature: TokenStream = signature.into();
    signature.extend([Group::new(Delim::Brace, TokenStream::new()).into_token_tree()]);
    match native_impl(meta.into(), signature) {
        Ok((head, bindings)) => {
            let mut output: proc_macro::TokenStream = head.into();
            output.extend([proc_macro::TokenTree::Group(body)]);
            output.extend(proc_macro::TokenStream::from(bindings));
            output
        }
        Err(error) => error.to_compile_error().into(),
    }
}
fn native_impl(
    meta: TokenStream,
    item: TokenStream,
) -> Result<(TokenStream, TokenStream), ParseError> {
    let opts = options(meta, &["symbol", "abi", "effects", "capabilities"])?;
    let mut function: ItemFn = moxy::parse!(item)?;
    if function.sig.asyncness.is_some()
        || function.sig.safety.is_some()
        || !function.sig.generics.params.is_empty()
        || function.sig.params.variadic.is_some()
    {
        return Err(ParseError::new(
            function.sig.span(),
            "export a safe, monomorphic synchronous wrapper; resumable natives use HostTicket",
        ));
    }
    let symbol_meta = find(&opts, "symbol").ok_or_else(|| {
        ParseError::new(
            function.sig.ident.span(),
            "native requires symbol = \"module::function\"",
        )
    })?;
    let symbol: String = symbol_meta.parse()?;
    if !symbol
        .rsplit_once("::")
        .is_some_and(|(a, b)| !a.is_empty() && !b.is_empty())
    {
        return Err(ParseError::new(
            symbol_meta.span(),
            "expected module::function symbol",
        ));
    }
    let symbol = LitStr::new(&symbol, symbol_meta.span());
    let abi = find(&opts, "abi")
        .map(expression)
        .transpose()?
        .unwrap_or_else(|| moxy::template! { 1 });
    let effects = find(&opts, "effects").map(expression).transpose()?.unwrap_or_else(||
        moxy::template! { ::aside_host::aside_vm::bytecode::Effects(::aside_host::aside_vm::bytecode::Effects::ALLOCATE) });
    let capabilities = find(&opts, "capabilities")
        .map(expression)
        .transpose()?
        .unwrap_or_else(|| moxy::template! { [] as [&str; 0] });
    let mut host = moxy::template! { () };
    let mut context_seen = false;
    let mut params = Vec::new();
    let mut reads = Vec::new();
    let mut arguments = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    for param in function.sig.params.inputs.iter_mut() {
        let FnParam::Typed(param) = param else {
            return Err(ParseError::new(
                param.span(),
                "expected a named typed parameter",
            ));
        };
        let Pattern::Ident(pattern) = param.pat.as_ref() else {
            return Err(ParseError::new(
                param.span(),
                "native parameters need an identifier",
            ));
        };
        let ident = pattern.ident.clone();
        let opts = helper(&param.attrs, &["name", "default", "context"])?;
        param.attrs.retain(|a| !a.path.is_ident("aside"));
        if flag(&opts, "context")? {
            let Type::Reference(reference) = param.ty.as_ref() else {
                return Err(ParseError::new(
                    param.ty.span(),
                    "context requires &mut CallContext<'_, Host>",
                ));
            };
            let Some(types) = type_arguments(&reference.elem, "CallContext") else {
                return Err(ParseError::new(
                    param.ty.span(),
                    "context requires &mut CallContext<'_, Host>",
                ));
            };
            if types.len() != 1 || reference.mutability.is_none() || context_seen || opts.len() != 1
            {
                return Err(ParseError::new(
                    param.span(),
                    "one mutable context parameter is allowed",
                ));
            }
            host = types[0].to_token_stream();
            context_seen = true;
            arguments.push(moxy::template! { &mut __aside_context });
            continue;
        }
        let name = find(&opts, "name")
            .map(|m| m.parse::<String>())
            .transpose()?
            .unwrap_or_else(|| ident.text().to_owned());
        if !names.insert(name.clone()) {
            return Err(ParseError::new(
                param.span(),
                "duplicate native parameter name",
            ));
        }
        let name = LitStr::new(&name, ident.span());
        let argument = moxy::token::Ident::new(format!("__aside_argument_{}", params.len()));
        let ty = &param.ty;
        let default = if let Some(meta) = find(&opts, "default") {
            let value = constant(meta)?;
            moxy::template! { Some({{ value }}) }
        } else {
            moxy::template! { None }
        };
        let index = literal(params.len());
        params.push(moxy::template! {
            ::aside_host::NativeParameter { name: {{ name }}.into(),
                ty: <{{ ty }} as ::aside_host::FromValue>::TYPE, default: {{ default }} }
        });
        reads.push(moxy::template! {
            let {{ &argument }} = <{{ ty }} as ::aside_host::FromValue>::from_value(__aside_vm.heap(),
                __aside_arguments[{{ index }}])?;
        });
        arguments.push(moxy::template! { {{ argument }} });
    }
    let (result, error, fallible) = match &function.sig.output {
        ReturnType::Default => (
            moxy::template! { () },
            moxy::template! { ::aside_host::Error },
            false,
        ),
        ReturnType::Type(_, ty) => {
            if let Some(types) = type_arguments(ty, "Result") {
                if types.len() != 2 {
                    return Err(ParseError::new(
                        ty.span(),
                        "use Result<Value, Error> with an explicit error type",
                    ));
                }
                (types[0].to_token_stream(), types[1].to_token_stream(), true)
            } else {
                (
                    ty.to_token_stream(),
                    moxy::template! { ::aside_host::Error },
                    false,
                )
            }
        }
    };
    let name = &function.sig.ident;
    let vis = &function.vis;
    let count = literal(params.len());
    let attrs = &function.attrs;
    let gates: Vec<_> = attrs
        .iter()
        .filter(|attr| attr.path.is_ident("cfg"))
        .collect();
    let signature = &function.sig;
    let unwrap = if fallible {
        moxy::template! { ? }
    } else {
        TokenStream::new()
    };
    let head = moxy::template! { {{ attrs }} {{ vis }} {{ signature }} };
    let bindings = moxy::template! {
        @for gate in &gates { {{ gate }} }
        {{ vis }} mod {{ name }} {
            use super::*;
            pub const SYMBOL: &str = {{ &symbol }};
            pub fn binding() -> ::aside_host::NativeBinding<{{ &host }}, {{ &error }}> {
                ::aside_host::NativeBinding {
                    contract: ::aside_host::NativeContract {
                        symbol: SYMBOL.into(), abi_version: {{ abi }},
                        parameters: vec![ @for p in &params { {{ p }}, } ],
                        result: <{{ result }} as ::aside_host::IntoValue>::TYPE,
                        effects: {{ effects }},
                        required_capabilities: ({{ capabilities }}).into_iter().map(|v: &str| v.into()).collect(),
                    },
                    invoke,
                }
            }
            pub fn invoke(__aside_host: &mut {{ host }}, __aside_vm: &mut ::aside_host::aside_vm::executor::Vm,
                __aside_ticket: ::aside_host::aside_vm::executor::HostTicket) -> Result<(), {{ error }}> {
                let __aside_arguments = __aside_vm.host_arguments(__aside_ticket)?;
                if __aside_arguments.len() != {{ count }} {
                    return Err(::aside_host::Error::Arguments.into());
                }
                @for read in &reads { {{ read }} }
                @if context_seen {
                    let mut __aside_context = ::aside_host::CallContext {
                        host: __aside_host, vm: __aside_vm, ticket: __aside_ticket,
                    };
                }
                let __aside_result = super::{{ name }}(@for arg in &arguments { {{ arg }}, }) {{ unwrap }};
                ::aside_host::respond(__aside_vm, __aside_ticket, __aside_result)?;
                Ok(())
            }
        }
    };
    Ok((head, bindings))
}

/// Describe a stable, fieldless property identity enum without duplicating UI data.
#[proc_macro_derive(Properties, attributes(aside))]
pub fn properties(item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input: TokenStream = item.into();
    let result = moxy::parse!(input).and_then(properties_impl);
    match result {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}
fn properties_impl(item: ItemEnum) -> Result<TokenStream, ParseError> {
    if !item.generics.params.is_empty() {
        return Err(ParseError::new(
            item.span(),
            "a property schema is monomorphic",
        ));
    }
    let mut entries = Vec::new();
    let mut identities = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    for (index, variant) in item.variants.iter().enumerate() {
        if !matches!(variant.fields, Fields::Unit) || variant.discriminant.is_some() {
            return Err(ParseError::new(
                variant.span(),
                "property identities must be unit variants without discriminants",
            ));
        }
        let opts = helper(
            &variant.attrs,
            &[
                "name",
                "ty",
                "default",
                "animatable",
                "readonly",
                "validate",
            ],
        )?;
        let ident = &variant.ident;
        let name = find(&opts, "name")
            .map(|m| m.parse::<String>())
            .transpose()?
            .unwrap_or_else(|| ident.text().to_lowercase());
        if !names.insert(name.clone()) {
            return Err(ParseError::new(variant.span(), "duplicate property name"));
        }
        let name = LitStr::new(&name, ident.span());
        let ty = find(&opts, "ty")
            .map(expression)
            .transpose()?
            .unwrap_or_else(|| moxy::template! { Any });
        let default = find(&opts, "default")
            .map(constant)
            .transpose()?
            .unwrap_or_else(|| moxy::template! { ::aside_host::aside_vm::bytecode::Const::Null });
        let animated = flag(&opts, "animatable")?;
        let writable = !flag(&opts, "readonly")?;
        if animated && !writable {
            return Err(ParseError::new(
                variant.span(),
                "a readonly property cannot be animated",
            ));
        }
        let validator = if let Some(meta) = find(&opts, "validate") {
            let expr = expression(meta)?;
            moxy::template! { Some({{ expr }}) }
        } else {
            moxy::template! { None }
        };
        let writable = moxy::token::LitBool::new(writable, Span::call_site());
        let animated = moxy::token::LitBool::new(animated, Span::call_site());
        let index = literal(index);
        entries.push(moxy::template! { ::aside_host::Property { name: {{ name }},
        ty: ::aside_host::aside_vm::bytecode::BuiltinType::{{ ty }}, default: || {{ default }},
        writable: {{ writable }}, animatable: {{ animated }}, validator: {{ validator }} } });
        identities.push(moxy::template! { Self::{{ ident }} => {{ index }}, });
    }
    let ident = &item.ident;
    Ok(moxy::template! {
        impl ::aside_host::PropertySchema for {{ ident }} {
            const PROPERTIES: &'static [::aside_host::Property] = &[ @for entry in &entries { {{ entry }}, } ];
        }
        impl {{ ident }} {
            pub const fn index(self) -> usize { match self { @for identity in &identities { {{ identity }} } } }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_errors_point_to_invalid_options_before_codegen() {
        let error = native_impl(
            moxy::template! { symbol = "sample::wrong" },
            moxy::template! { fn wrong(#[aside(name = "same")] a: i64, #[aside(name = "same")] b: i64) {} },
        ).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("duplicate native parameter name")
        );
        let input = moxy::template! { enum Wrong { #[aside(readonly, animatable)] X } };
        let error = properties_impl(moxy::parse!(input).unwrap()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("readonly property cannot be animated")
        );
        let error = options(moxy::template! { unknown = true }, &["symbol"])
            .err()
            .unwrap();
        assert!(error.to_string().contains("unknown Aside binding option"));
    }
}
