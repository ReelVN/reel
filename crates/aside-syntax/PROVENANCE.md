# Aside syntax provenance

The initial implementation came from Rowan 0.17.0, revision
`c781c336f44a28210fefaa4d86b8c9902b781c8e` of
[rust-analyzer/rowan](https://github.com/rust-analyzer/rowan), imported on
2026-10-13. Original MIT/Apache-2.0 licenses and source copyright notices remain;
the internal Arc implementation retains Triomphe attribution.

Reel maintains the adopted library sources as `aside-syntax`. Local changes
replace recursive green-tree destruction with an explicit queue and atomic
final-owner decision. Token cursors traverse recovery subtrees iteratively while
preserving zero-length tokens and subtree boundaries.
