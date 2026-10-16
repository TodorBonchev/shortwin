#!/usr/bin/env bash
# Builds the TEST-ONLY `test-venue` variant of short.win (ARCHITECTURE §8.2, §9 M2; A49):
# `VenueKind::Mock`, the adapter of the mock venue tests/mock_cpamm, which the LiteSVM engine
# tests load. It never replaces the release artifact:
#
#   release:     anchor build --tools-version v1.57 --arch v3           -> target/deploy/shortwin.so
#   test-venue:  scripts/build-test-venue.sh                             -> target/test-venue/deploy/shortwin.so
#
# The test-venue build runs in its own cargo target directory (default target/test-venue,
# override with SHORTWIN_TV_TARGET), so the two builds never share an output path or a
# fingerprint. The LiteSVM harness loads this file through `Env::new_test_venue()`
# (tests/litesvm/src/env.rs; override with SHORTWIN_TEST_VENUE_SO). CI checks that the release
# shortwin.so carries neither the mock id nor the test-venue marker, and that this build does.
#
# Always the pinned toolchain: anchor 1.2.0, platform-tools v1.57, SBPF v3 (docs/DEVELOPMENT.md).
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
target=${SHORTWIN_TV_TARGET:-$root/target/test-venue}
tools=${PLATFORM_TOOLS:-v1.57}
arch=${SBPF_ARCH:-v3}

cd "$root"
echo "test-venue build -> $target/deploy/shortwin.so (platform-tools $tools, SBPF $arch)"
CARGO_TARGET_DIR="$target" anchor build --tools-version "$tools" --arch "$arch" \
  --ignore-keys --no-idl -p shortwin -- --features test-venue

so="$target/deploy/shortwin.so"
flags=$(od -An -t x4 -j 48 -N 4 "$so" | tr -d ' ')
if [ "$flags" != "00000003" ]; then
  echo "error: $so e_flags=$flags, expected SBPF v3 (00000003)" >&2
  exit 1
fi
if ! LC_ALL=C grep -aq "shortwin:test-venue:mock-cpamm" "$so"; then
  echo "error: $so lacks the test-venue marker: was the feature enabled?" >&2
  exit 1
fi
sha256sum "$so"
