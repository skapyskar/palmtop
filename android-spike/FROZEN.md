# This directory is historical, not actively maintained

Real client development moved to `../android/` (a proper Gradle project) specifically so
real dependencies -- like the Noise Protocol library used for transport encryption -- resolve
normally through Maven Central instead of being vendored by hand. See the top-level
`README.md`'s "Client build: migrated to Gradle" section for why.

What's still here and still true:
- `test-1080p.h264` generation and the manual `aapt2`/`d8`/`apksigner` `build.sh` pipeline are
  a real record of how the Phase 0 decode spike was built and proven *without* a Gradle
  download, when that was the right tradeoff for a quick feasibility check.
- The Java sources here predate Noise encryption, in-app discovery's pubkey field, and some
  later fixes -- they will not build a working client against a current `palmtopd`.

Don't extend this directory. If something here is missing from `../android/`, port it there.
