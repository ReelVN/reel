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

Known sections set flags=1 (required when present). Kinds are unique; nonempty
ranges are disjoint and outside the header/index. Unknown required sections
fail. Full decoding hash-checks all payloads, including optional unknown sections.
Writers emit header, bytecode, metadata, optional distribution and index.

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
