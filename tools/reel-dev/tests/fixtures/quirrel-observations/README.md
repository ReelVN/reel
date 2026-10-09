# Quirrel observation integrity fixture

These are immutable test inputs extracted from the accepted T01 observations in
`21e0ea09ddbd6c3926df72a6689183bb73b21598:tools/quirrel-compat/results`.
They retain the original eight mismatches and three seed runs of one passing
fixture, with their raw SHA-256 values, original golden references and readable
failure bytes. `actual.zip` contains only those eleven observations.

`reel-dev` tests verify original comparison semantics, all hashes, exit codes,
and rejection of forged hashes/statuses/returns and changed source/goldens.
They do not claim execution coverage. Complete run bundles are generated under
`target/reel-artifacts` and checked with `reel-dev verify-results DIR`.
Upstream source/golden license: `third_party/quirrel/COPYRIGHT` (MIT).
