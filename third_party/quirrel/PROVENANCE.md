# Quirrel reference snapshot

Upstream: <https://github.com/GaijinEntertainment/quirrel>
Commit: `19f17df9b6ee43c91ef5c635fd45970ec76c5dbe` (Quirrel 4.41).
Imported 2026-10-08 for Reel Issue #13. Development reference only.

`UPSTREAM.json` lists the selected upstream paths and SHA-256 of every imported
file. It includes all sources needed by the upstream CMake `sq` target, Sqrat
headers, the complete `testData` and `doc/examples` trees, original goldens and
`testRunner.py`. `doc/build.py`, `doc/README.md`, and `appveyor.yml` preserve the
upstream build instructions; the documentation site generator is not imported.
Other site content, deployment workflows and Git history are omitted.

Local changes to upstream files: **none**. This file and `UPSTREAM.json` are Reel
metadata. No nested `.git`; no runtime dependency or player linkage.

License: original MIT notices in `COPYRIGHT`, `squirrel/compiler/COPYRIGHT`,
Sqrat's zlib notices in its header files, and other source headers are retained. Converted derivatives
refer to this snapshot and its license.

Reproduction and actual results: [compatibility tools](../../tools/quirrel-compat/README.md).
