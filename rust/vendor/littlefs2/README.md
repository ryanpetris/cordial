# LittleFS runtime geometry

This is `littlefs2` 0.8.1 from the published upstream crate, under its Apache-2.0
or MIT license. `littlefs2-sys` and LittleFS itself remain registry dependencies.

The local patch adds `Storage::block_count()` for runtime formatting geometry.
Mount reads the persisted block count without treating a smaller driver range as
filesystem corruption. Drivers enforce bounds on individual accesses. Geometry
and callback offsets must fit the byte-addressed interface without wrapping.
`Filesystem::grow` accepts growth within the driver range. Total and available
space use the mounted block count.

The patch also adds `Filesystem::make_consistent()` to resolve interrupted
LittleFS operations before Cordial reads application data or removes temporary
files. This prevents an interrupted cross-directory rename from hiding entries
during startup validation. Recovery may write metadata; a recovery failure stops
startup without formatting the filesystem.

Cordial supplies block-aligned bounds and validates the application epoch before
using or growing a filesystem. The record-storage fixtures exercise runtime
sizes, growth, recovery and interrupted writes.
