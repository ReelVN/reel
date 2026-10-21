# Reel ASB 1.0

ASB is Reel's code container. Aside bytecode remains independent of filesystem,
distribution, and player configuration. The reader accepts version 1.0.

All integers are little-endian. The 72-byte header contains `REELASB\0`,
`major:u16`, `minor:u16`, `flags:u32`, `total_len:u64`, `index_offset:u64`,
`index_len:u64`, and the 32-byte BLAKE3 index digest. Header flags are zero.
Each 56-byte index record contains `kind:u32`, `flags:u32`, `offset:u64`,
`length:u64`, and a 32-byte BLAKE3 payload digest. Offsets are file-relative.

| Kind | Payload | Presence |
| --- | --- | --- |
| 1 | Verified Aside bytecode | Required |
| 2 | UTF-8 JSON code metadata, schema 1 | Required |
| 3 | UTF-8 JSON `Distribution` | Present in exported projects |

Every known section has flags=1 (required when present). Section kinds are
unique. Ranges must be nonempty, disjoint, and outside the header and index.
Unknown required sections fail; unknown optional sections are hash-checked by
full decoding. Writers produce header, bytecode, metadata, optional distribution,
then index, without padding or timestamps.

`Distribution` contains `text`, ordered `resources`, `loose`, and `source`.
Paths are relative to the containing ASB directory and validated at admission.
`source` contains the editable entry file, module roots, and entry function.
Exported releases reference the existing RTP text pool and RPK resource volumes;
development exports reference copied source and loose assets. Ordinary code-only
builds omit the distribution section. Execution identity comes from the code and
its original text pool, so distribution paths do not become VM state.

Full decoding verifies all section digests, code metadata, and bytecode once.
The seekable distribution reader reads only the header, index, and distribution
section; it checks their ranges, limits, digests, JSON structure, and paths.
Both readers use the same container index validation. JSON limits apply to both
metadata and distribution. Neither reader scans resource archive payloads.
