# Aside source frontend

The frontend provides lossless syntax trees, source ranges and bounded diagnostics.
The [language specification](../../docs/aside-language.md) defines the grammar;
[README.md](README.md) describes semantic analysis, lowering and validation.
The compiler uses aside-vm's bytecode model and remains independent of Reel and
platform libraries.

Public API: `lex(&str) -> Result<Lexed, SourceTooLarge>`,
`lex_with_limits(&str, Limits)`, `parse(&str) -> Result<Parse, SourceTooLarge>`,
`parse_with_options(&str, ParseOptions)` and
`EntryPoint::{Module, Block, Expression, Pattern, Type}`. Block entry accepts a
function-body fragment without shifting source ranges. One parser handles all
entries; children uses the ordinary block grammar. Source admission rejects the
configured byte limit or the syntax tree's u32 offset capacity before allocation. Other resource
exhaustion remains a Parse with diagnostics, an exhaustion flag and a lossless
error tail.

`Token { kind: SyntaxKind, range: TextRange }` references the caller's UTF-8
source, including whitespace/comments. `Lexed` owns token ranges and lexical
diagnostics, not duplicate source strings. `Parse` owns an `aside-syntax` green tree;
`green()/syntax()` expose immutable/navigation forms. Every admitted source byte
is represented by a leaf, including invalid input. Missing tokens are never
invented in the user's source. Budgets cap recursive descent and nested syntax,
tokens and diagnostics; input truncation always makes progress or exits. Ordinary
tokens obey the exact emission limit, with at most one separate error-tail leaf.
Flat left-associative operator and postfix chains use explicit work lists and do
not consume one nesting level per segment. Recursive depth defaults to 128 and
honors the caller's configured value without a hidden maximum clamp.

Ambiguous assignment targets use a silent, isolated trial of the same pattern,
type and default-expression productions. Tokens/source are borrowed; caller
cursor, diagnostics and tree are untouched. Only a complete structural pattern
followed by `=` selects pattern parsing; sole grouped names remain locations,
and member/index suffixes use expressions. Tuple/array expression nodes cannot
serve as a fallback destructuring target containing setters. Type/OR wrapping
and arbitrary grouping retain the pattern grammar; mutable-name and OR-binding
eligibility remain binder checks.

All candidates in one parse share at most 16 times the admitted token count in
trial token advances, including nested default expressions. Completed grammar
decisions are cached by token position/pipe context (at most two entries per
position); depth uses the same normalized budget, including the caller depth.
Trials stop on their first error. Budget exhaustion reports ASR001 at the real
candidate and preserves the untouched tail; failed trials emit no author-facing
diagnostics. No second pattern grammar or per-candidate file clone is used.

Diagnostics expose stable code, severity, primary/related half-open byte ranges,
messages and optional fixes. `to_codespan(file_id)` leaves file/module/version
identity with the caller. `SourceMap` exposes zero-based line and explicitly
named byte/scalar/UTF-16 columns. Terminal display uses codespan-reporting;
EOF errors use an empty range at the actual end. Syntax success is not binding,
type/effect checking, lowering, package verification or execution.

## Owner-resolved author syntax

- Pattern type test (aside-language.md:114): `pattern is Type`; is binds more
  tightly than pattern OR `|`. A union type is explicitly parenthesized,
  `value is (A|null)`. Equal OR binding names are checked by the later binder.
- Function types: `fn(T, U) -> R`, arrows right associative and return
  types may be unions. A function type participating in an outer union is
  grouped, `(fn(T) -> R)|null`. `(T)` is a grouped type; a comma identifies tuple
  types `(T,)` / `(T,U)` (and `()` is empty tuple).
- Closure types: the outer parameter `|` closes the parameter
  list; a union annotation must be grouped, `|x: (A|null)| body`. No speculative
  lookahead guesses whether a body name is part of the type.
- Yield: its operand includes ordinary binary/range expressions but
  excludes assignment, so yield binds above assignment and below range.
  `let incoming = yield outgoing + 1;` is valid; `yield a = b;` is an invalid
  assignment target. `yield;` yields null.
- Assignment: `a = b = value;` is a right-associative assignment chain
  statement, with no expression result. Argument/arithmetic/let-initializer
  assignment still diagnoses the actual assignment operator. Evaluation order
  and inner-to-outer writes belong to lowering/execution; parsing claims neither
  transactions nor executed side effects.
- Class values: anonymous expressions are `class { ... }` or
  `class: Parent { ... }`. Named declarations remain `class Name: Parent`, and
  their names may be used as values. Internally named class expressions are not
  added.
- Match arms: outer bare `{ ... }` is a statement block with an
  optional final value; object results are grouped, `_ => ({name: value})`.
  This shares closure/block rules.
- Inside a statement block, an object field prefix (`{name:`, `{"key":`,
  `{[expression]:`) or spread prefix (`{...`) selects the ordinary object
  expression, including as the block's final value. A label followed by
  while/for/loop keeps its block meaning. Empty `{}` remains a statement block;
  use `({})` for an empty object value. Required outer blocks and trailing
  children keep their grammar.
- Control heads: outer if/while/for/match heads disable trailing
  children; explicit parentheses restore ordinary expressions. Proven pairs:
  `if ready() { go(); }` / `if (ready() { node; }) { go(); }`. Class base
  expressions use the same boundary, `class: factory() { ... }`; group a
  trailing-children base expression explicitly.
