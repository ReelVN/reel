# Aside source frontend — interface draft

Design 1.1, Issue #8. Allowed changes: this directory and its Cargo.lock entries.
Workspace/toolchain dependency: D01 `issue-1-core-contracts` at
`c03226a59a9dd4f8b448d4eea82262a1d6e57ef8` (Rust 1.96.0).
No dependency on aside-vm, Reel, platform libraries or bytecode.

The intended public API is `lex(&str) -> Lexed`, `parse(&str) -> Parse`,
`parse_with_options(&str, ParseOptions) -> Parse` and `EntryPoint::{Module,
Block, Expression, Pattern, Type}`. One parser handles every entry point;
children uses the ordinary block grammar.

`Token { kind: SyntaxKind, range: TextRange }` references the caller's UTF-8
source, including whitespace/comments. `Lexed` owns token ranges and lexical
diagnostics, not duplicate source strings. `Parse` owns a rowan green tree,
diagnostics, and a flag distinguishing resource termination from completed
(recovering) parsing. `Parse::syntax()` exposes a typed-language rowan root.
Every consumed source byte is represented by a leaf, including invalid input.
No synthetic missing token is claimed to exist in the user's source.

Diagnostics have stable codes, severity, primary and related half-open byte
ranges, messages and optional fixes. A conversion to codespan-reporting accepts
a caller-owned file ID; the caller attaches module/version identity. Positions
expose zero-based line, UTF-8 byte column, Unicode scalar column and UTF-16 column
for editor use. Terminal display follows codespan-reporting. Errors at EOF use
an empty range at the actual end. Budget limits cap recursive descent, tokens
and diagnostics; overflow remains losslessly preserved as an error node.

Unresolved author syntax (reported before implementation):

- docs/aside-language.md:114 names type-test patterns without their syntax.
  Proposed form: `pattern is Type`.
- docs/aside-language.md:163 names function-signature types without their syntax.
  Proposed form: `fn(T, U) -> R`.
- docs/aside-language.md:210 calls yield an expression of low precedence, but
  does not order it relative to assignment. Assignment has no expression value
  (line 61); the interaction needs a defined rejection/precedence rule.
- docs/aside-language.md:136 permits class values, but does not specify whether
  anonymous `class [: Parent] { ... }` is allowed.

These are specification gaps, not reasons to omit defined tuple, class,
closure, children, import or pattern syntax. Undefined forms remain explicitly
pending owner resolution; no alternate UI grammar or semantic executor is added.

Additional owner review points (reported during implementation):

- Line 19 defines `|x| expression`; line 163 allows union annotations `A|null`.
  `|x: A|null| body` uses identical tokens for union and parameter-list close.
  Candidate A: require a parenthesized union in closure annotations,
  `|x: (A|null)| body`, while `|x: A| body` stays valid. Candidate B: specify a
  terminating-pipe rule (including the body starting with a name/closure), then
  support unparenthesized union. No lookahead based on runtime meaning.
- Line 114 says OR patterns but gives no operator spelling. Candidate: `a | b`;
  each side uses the same recursive pattern grammar and local binding checks
  belong to the later binding pass. Provisional OR/type-test/function-type/
  anonymous-class trees currently carry `ASP015`, never successful acceptance.
- Line 61 specifies right-associative assignment and no value usable in
  arithmetic. Candidate A: permit `a = b = c;` as a special chain statement,
  while rejecting assignment in arithmetic/argument/initializer values.
  Candidate B: parse right associativity but diagnose chains as lacking a value.
- Lines 25/124: the outermost if/while/for/match head disables trailing children;
  explicit parentheses restore ordinary expressions. Proven pairs include
  `if ready() { go(); }` and `if (ready() { node; }) { go(); }`.
- Line 25 says expression-position braces are objects; line 119 uses match-arm
  expression results. Current defined interpretation: `_ => {name: value}`
  is an object expression; ordinary statement blocks remain valid as statements
  inside children/control-flow bodies. If match arms are intended to create a
  block context, specify that explicitly and use `_ => ({name: value})` for an
  object result. This needs review before accepting unparenthesized arm braces.

Source admission refinement: public lex/parse functions return
`Result<Lexed/Parse, SourceTooLarge>` to reject source exceeding the configured
byte limit or rowan's u32 byte capacity before allocation. Token/depth/diagnostic
exhaustion remains an ordinary `Parse` with exact-range diagnostics and a
lossless error tail. Oversize admission is never claimed to produce a tree.
