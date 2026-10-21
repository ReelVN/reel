# Source provenance

The initial syntax-tree implementation comes from Rowan 0.17.0, published from
revision `c781c336f44a28210fefaa4d86b8c9902b781c8e` of
<https://github.com/rust-analyzer/rowan>, imported on 2026-10-13. Original MIT and
Apache-2.0 licenses and source copyright notices are retained. The internal Arc
implementation preserves its original Triomphe attribution.

Reel owns subsequent maintenance under the independent `aside-syntax` workspace
package and `aside_syntax` Rust library name. It has no crates.io patch, parallel
Rowan dependency, source-hash gate, upstream layout obligation, or synchronization
requirement. Only library sources and necessary license material were adopted;
upstream examples and repository maintenance tooling were not imported.

The first ownership correction replaces recursive green-tree destruction with
an explicit queue. A consuming atomic decrement determines the actual final
reference, including concurrent releases; the final owner keeps direct children
alive before freeing their parent. This preserves node layouts and raw cursor
ownership while allowing long author expressions to be released safely. Existing
compiler frontend checks cover a 5000-operation chain, retained cursors, and
concurrent final green-tree clones; compiler execution checks cover source
semantics and GC independently.

Token cursors traverse empty recovery subtrees iteratively while preserving
zero-length tokens and the requested subtree boundary.
