# Prepared images, format 1

`reel prepare-image original.png --output panorama.reel-image` prepares an original
PNG/JPEG for `image("panorama.reel-image")`. The command writes an immutable manifest
and its adjacent `.tiles` directory. Ship both.

Run [author_large_image.aside](../examples/author_large_image.aside) with a wide
original and the font preparation from the creator guide. It pans, zooms out/in,
then revisits the panorama using common stage transforms. Preparation is currently
explicit; ASB build does not prepare or package image assets automatically.

The 24-byte little-endian manifest contains `REELIMG1`, original width, original
height, interior tile edge, and level count (four u32 values). Dimensions and tile
edge are positive. Levels repeatedly halve dimensions, rounding upward, through
1×1. Level N uses `panorama.reel-image.tiles/N/X-Y.png`. Each tile includes one
neighbor pixel on all sides, clamped at image boundaries. Tile dimensions are
interior dimensions plus two. Preparation uses the workspace-pinned image codec;
it decodes the original offline, never in response to runtime camera movement.
Downsampling filters linear premultiplied colors and stores ordinary sRGB PNGs.
Preparation requires memory for the original and floating-point filter workspace;
runtime memory depends on the view. `--tile-size` defaults to 512 interior pixels.
The tile edge plus two gutter pixels must fit the target device's texture limit.

The player reads manifests through AssetIo and resolves original dimensions before
layout. It computes tile demand from the existing composed geometry, window bounds,
ancestor clips, object fit and physical scale. Camera, scroll and ordinary node
transforms therefore use the same path. Required tiles use the finest pyramid level
whose texels are approximately physical pixels; a one-tile neighbor ring is optional
prefetch. Multiple nodes share resource keys and leases. Admission follows configured
byte budgets, without a fixed image/tile count limit.

Missing required tiles keep the last accepted GPU frame. Input and
`stage.presented()` advance after the required tiles are ready.
Superseded requests are cancelled, late results cannot enter the new demand, and
retirement releases image leases through the existing resource lifecycle. Optional
prefetch is evicted/deferred before required loading. Accepted and pending frames
retain their actual leases and budget charges. Painting fills interior rectangles
directly on the shared target while the one-pixel neighbor gutter supplies bilinear
samples across tile boundaries. Tile interiors do not use separate clip layers;
adjacent antialiased edges must share rasterization before group compositing.

The format lives in reel-core, preparation in reel-cli, loading and layout in
reel-player, and image painting in reel-ui.
Checks cover malformed manifests, odd-size pyramids/gutters, transformed visibility,
cold/warm pan and zoom, cancellation, residency release and actual byte/load counts.
GPU pixel and device-limit verification is target-specific and reported separately.

Run the contracts with `cargo test --locked -p reel-core --lib prepared_image`,
`cargo test --locked -p reel-cli --lib prepare_image -- --nocapture`, and
`cargo test --locked -p reel-player --lib image -- --nocapture`.
On a graphics target, run
`cargo test --locked -p reel-player --features window --lib prepared_tiles_match_contiguous_image_through_rotated_camera -- --ignored --nocapture`.
It compares adjoining tiles against a contiguous image through real rotated camera
views and reports decoded working-set bytes, GPU residency/upload reservation and
warm reuse. The synthetic two-dimensional source exceeds the tested device limit;
only a local tile neighborhood is generated for this check.
Generated originals, pyramids and logs belong under ignored `target/reel-artifacts`.
