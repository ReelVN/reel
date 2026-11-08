# Reel ASB v1

ASB contains verified Aside code, source metadata and optional distribution
configuration. The encoded version is major 1, minor 0. Model/validation details
are documented in [the package sources](src/lib.rs).

All integers are little-endian. The 72-byte header contains `REELASB\0`,
`major:u16`, `minor:u16`, `flags:u32`, `total_len:u64`, `index_offset:u64`,
`index_len:u64` and a 32-byte BLAKE3 index digest. Header flags are zero.
Each 56-byte index record contains `kind:u32`, `flags:u32`, `offset:u64`,
`length:u64` and a 32-byte BLAKE3 payload digest. Offsets are file-relative.

| Kind | Payload | Presence |
| --- | --- | --- |
| 1 | Verified Aside bytecode | Required |
| 2 | UTF-8 JSON code metadata, schema 1 | Required |
| 3 | UTF-8 JSON `Distribution` | Exported projects |
| 4 | Effect definitions and linked function bindings | Projects using compiled effects |
| 5 | UTF-8 JSON logical continuation table, version 1 | Projects declaring stable resume flows |

Known sections set flags=1 (required when present). Kinds are unique; nonempty
ranges are disjoint and outside the header/index. Unknown required sections
fail. Full decoding hash-checks all payloads, including optional unknown sections.
Writers emit header, bytecode, metadata, optional distribution, optional effects,
optional logical continuations and index.
Container writers receive immutable serialized bytecode paired with its verified
program and validate metadata against that shared program.

`Distribution` holds optional `text`, ordered `resources`, `catalogs` and optional
`loose`. File paths are relative to the ASB directory and validated at admission.
`catalogs` maps canonical language tags to logical entries in the text archive;
a nonempty catalog index requires `text`. `loose` is `Project` (resolve the
surrounding project resources) or `Directory(path)` (resolve relative to the ASB).
Releases reference RTP text and RPK resource volumes; development exports use
loose assets. Code-only builds can omit the section. Code and its original text
pool determine execution identity. The JSON schema is declared in
[distribution.rs](src/distribution.rs).

Full decoding verifies section digests, metadata and bytecode. The seekable
distribution reader checks only header/index/distribution using the same index,
JSON, path and limit validation. Resource archives are read by their owning
loaders. Wire encoding is in [codec.rs](src/codec.rs).

## Effect products v1

Section 4 starts with `definition_count:u32, binding_count:u32`. Definitions
follow in table order, each encoded as `blake3:[u8;32], length:u64, bytes:[u8;length]`.
Each following binding is `module:u32, function:u32, definition:u32`, using final
linked bytecode ids and a zero-based definition-table index. Trailing data is
invalid. Definition digests and function bindings are unique; every definition
must be referenced. The bound function belongs to the specified module, is Plain,
and has no captures. The package validates lengths, digests and these bytecode
relationships before returning opaque products. Existing metadata-byte, record
and work budgets apply. The engine validates the typed definition before runtime
admission; the package has no UI dependency.

Canonical definition bytes start with `REELFXD1`, followed by the postcard tuple
`(parameters, nodes, output)` using the shared UI effect declarations' stable
field and variant order. Parameters include names, types and finite f32 defaults;
nodes retain finite f32 bits and preceding-node references. The digest covers the
whole definition including the prefix. Source paths, HIR function ids, source
spans, native handles and live instance values are excluded. The engine decoder
bounds table allocations and reconstructs the shared validated EffectProgram.
These products survive debug stripping and ASB re-encoding. GPU-target artifacts
are prepared separately from this portable definition identity.

## Logical continuations v1

Section 5 stores `aside_vm::resume::ResumeTable`. Stable flow and site keys map
to the current function ids, execution word PCs, result slots and live binding
layouts. The VM verifies the complete table against the decoded current program
before the package is admitted. The package retains that verified table; binding
an external text pool preserves the verification and shares the bound pool with
the execution program. JSON and resume verification use the package metadata and
work limits. An absent section denotes an empty version 1 table.

Continuation metadata survives release debug stripping, external text extraction
and distribution re-encoding. It describes current code; saved logical records
store stable keys and data independently of this numeric layout.

## Resource and text archives v1

RPK and RTP use the same seekable `REELPAK\0` v1 format. The 32-byte header is
`magic:[u8;8], version:u32, kind:u32, index_offset:u64, entry_count:u64`.
Kind is 1 for resources and 2 for text. The main index ends at EOF. Every sorted
entry is `path_len:u32, offset:u64, logical_length:u64, blake3:[u8;32],
stored_length:u64, directory_offset:u64, storage:u32`, followed by its UTF-8 path.
Offsets are absolute file offsets. The digest covers the logical input bytes.

Storage 0 is contiguous raw bytes: stored and logical lengths match and the
directory offset is zero. Storage 1 divides logical content into independent
64 KiB blocks, with a shorter final block. Stored block payloads precede their
directory. Each 16-byte directory record contains `relative_offset:u64,
stored_length:u32, codec:u32`. Offsets are relative to the entry's payload offset;
codec 0 stores the block raw and codec 1 stores a raw LZ4 block. Logical block
lengths derive from the entry length. Compressed blocks are strictly smaller
than their logical block. The directory contains `ceil(logical_length / 65536)`
records and immediately follows the stored payload region.

Mount reads only the header and main index, checking storage ranges and directory
sizes. Range reads fetch only addressed directory records and blocks, validate
their bounds and codec, and decode into a reusable 128 KiB workspace. Cancellation
is checked between IO chunks and blocks. The decoded half of that workspace
retains the most recently decoded block by logical content digest, length and
block index, so successive small range requests avoid repeated IO and decoding.
The previous cache identity is retired before a decode can overwrite its bytes;
only a successful exact-length decode publishes the new identity.
Writers accept unknown-length readers,
hash logical input incrementally, retain only block records and fixed payload
workspace, and choose raw storage for blocks that do not shrink. An entry with
no compressed blocks becomes a contiguous raw entry. Callers may explicitly
choose raw storage for already compressed content.
