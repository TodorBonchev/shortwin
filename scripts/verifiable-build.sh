#!/usr/bin/env bash
# Verifiable build of the deployed program, shortwin (ARCHITECTURE §1.1 "Verifiable build", C1-5;
# §9 M10), with solana-verify 0.5.2 in its pinned Agave 4.1.2 image. (The straddle helper
# complete_curve, built here until 1.0.0, is retired: design pump-final §11, owner decision D2.)
#
#   scripts/verifiable-build.sh [--local] [--out DIR] [--compare DIR]
#
# solana-verify picks the image from `[workspace.metadata.cli] solana = "4.1.2"` (Cargo.toml):
# solanafoundation/solana-verifiable-build@sha256:2e0b78f4… (its src/image_config.rs at v0.5.2 =
# Docker Hub tag 4.1.2): rust 1.95.0, cargo-build-sbf 4.1.0 from crates.io (the same crate
# Agave 4.1.2's installer ships, agave v4.1.2 scripts/cargo-build-sbf-version.sh), platform-tools
# v1.54 preinstalled. `--cargo-build-sbf-args="--tools-version v1.57"` (the `=` form is required:
# clap 2 `require_equals`) makes cargo-build-sbf fetch and use v1.57 inside the container, and
# `--arch v3` selects SBPF v3: the same compiler, flags and target as `anchor build --tools-version
# v1.57 --arch v3`.
#
# Modes:
#   default  real Docker (CI: GitHub's ubuntu-24.04 runners). Builds in this checkout; the
#            container runs as root, so target/ is chowned back afterwards.
#   --local  no Docker daemon: scripts/svb-shim/docker stands in for `docker`, running the SAME
#            image (fetched by digest from Docker Hub by scripts/svb-shim/fetch-image, every
#            blob's sha256 checked, extracted once into $SVB_CACHE, default
#            ~/.cache/shortwin/svb) under bubblewrap in an unprivileged user namespace. Builds a
#            fresh clone of HEAD in $SVB_CACHE/work (uncommitted changes are NOT built; the
#            checkout's target/deploy is not touched: outputs go to --out, default
#            target/verifiable). A container a failed build leaves behind (solana-verify skips its
#            `docker kill` when cargo build-sbf fails) is removed on exit. Needs bwrap, unprivileged user namespaces,
#            python3, ~8 GB on disk (a reflink file system such as btrfs makes each container
#            instant) and network (crates.io, platform-tools v1.57 from GitHub).
#
# Environment: SOLANA_VERIFY_VERSION, SVB_IMAGE_DIGEST (CI sets both at the top of ci.yml; the
# defaults below are the same pins), PLATFORM_TOOLS, SBPF_ARCH, SVB_CACHE, SVB_EXTRA_ENV,
# PR_HEAD_SHA (CI on pull_request: the PR head, recorded next to the merge commit it builds).
#
# Before building it asserts that no Cargo.toml in the build tree other than
# programs/<p>/Cargo.toml has the library name <p>: solana-verify's `--library-name` takes the
# FIRST match in `find` order, so a second crate named shortwin (a fork, a bench wrapper) would
# silently be built instead.
#
# Prints and writes DIR/verifiable-hashes.txt (default DIR: target/verifiable): per program the
# file's sha256 and solana-verify's executable hash (sha256 of the file with trailing zero bytes
# trimmed; what `get-program-hash`, `verify-from-repo` and verify.osec.io compare against the
# on-chain ProgramData). With --compare DIR (e.g. the anchor-build artifact's deploy/ directory)
# it fails unless every program is byte-identical to DIR/<program>.so.
set -euo pipefail

SOLANA_VERIFY_VERSION=${SOLANA_VERIFY_VERSION:-0.5.2}
SVB_REPO=solanafoundation/solana-verifiable-build
SVB_IMAGE_DIGEST=${SVB_IMAGE_DIGEST:-sha256:2e0b78f44ee76612e9260c7c988570c5e14de6fbd93e0ab07115ec7054473b4f}
PLATFORM_TOOLS=${PLATFORM_TOOLS:-v1.57}
SBPF_ARCH=${SBPF_ARCH:-v3}
PROGRAMS=(shortwin)

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
local_mode=0; out=""; compare=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --local) local_mode=1; shift ;;
    --out) out="$2"; shift 2 ;;
    --compare) compare=$(cd "$2" && pwd); shift 2 ;;
    *) echo "usage: $0 [--local] [--out DIR] [--compare DIR]" >&2; exit 2 ;;
  esac
done
out=$(mkdir -p "${out:-$root/target/verifiable}" && cd "${out:-$root/target/verifiable}" && pwd)

have=$(solana-verify --version 2>/dev/null | awk '{print $2}') || true
[[ "$have" == "$SOLANA_VERIFY_VERSION" ]] || {
  echo "error: solana-verify $SOLANA_VERIFY_VERSION required (found '${have:-none}'):" >&2
  echo "  cargo install solana-verify --version =$SOLANA_VERIFY_VERSION --locked" >&2; exit 1; }
grep -Eq '^solana = "4\.1\.2"' "$root/Cargo.toml" || {
  echo "error: Cargo.toml lacks [workspace.metadata.cli] solana = \"4.1.2\" (image selection)" >&2; exit 1; }

if [[ $local_mode == 1 ]]; then
  cache=${SVB_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/shortwin/svb}
  mkdir -p "$cache"; cache=$(cd "$cache" && pwd)
  "$root/scripts/svb-shim/fetch-image" "$SVB_REPO" "$SVB_IMAGE_DIGEST" "$cache" >/dev/null
  work="$cache/work"
  rm -rf "$work"; git clone -q "$root" "$work"
  echo "building HEAD $(git -C "$work" rev-parse HEAD) (local mode: committed state only)"
  build_dir="$work"
  export PATH="$root/scripts/svb-shim:$PATH" SVB_CACHE="$cache" SVB_IMAGE_DIGEST
  export SVB_EXTRA_ENV="${SVB_EXTRA_ENV:-CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(nproc)}}"
  # solana-verify 0.5.2 build() returns on "Failed to cargo build" before its `docker kill`; each
  # shim container is a full rootfs copy (~5 GB without reflinks). One run owns $cache (see work).
  trap 'for c in "$cache"/containers/svb*; do
          if [[ -e "$c" ]]; then "$root/scripts/svb-shim/docker" kill "${c##*/}" >/dev/null || true; fi
        done' EXIT
else
  docker pull -q "$SVB_REPO@$SVB_IMAGE_DIGEST"
  build_dir="$root"
fi

cd "$build_dir"
# The library-name lookup solana-verify 0.5.2 does (find_relative_manifest_path_and_build_path:
# `find <mount> -name Cargo.toml`, first manifest whose cargo_toml-completed lib name matches; the
# name is [lib] name, else the package name with - -> _ when there is a [lib] table or
# src/lib.rs). Each program's name must resolve to exactly its own manifest.
python3 - "$build_dir" "${PROGRAMS[@]}" <<'PY'
import os, subprocess, sys, tomllib
mount, programs = sys.argv[1], sys.argv[2:]
found = {p: [] for p in programs}
for path in subprocess.run(["find", mount, "-name", "Cargo.toml"], check=True,
                           capture_output=True, text=True).stdout.splitlines():
    try:
        with open(path, "rb") as f:
            m = tomllib.load(f)
    except (OSError, tomllib.TOMLDecodeError):
        continue
    lib, pkg = m.get("lib"), m.get("package")
    name = lib.get("name") if isinstance(lib, dict) else None
    if name is None and isinstance(pkg, dict) and isinstance(pkg.get("name"), str):
        has_lib = lib is not None or os.path.exists(os.path.join(os.path.dirname(path), "src", "lib.rs"))
        if has_lib:
            name = pkg["name"].replace("-", "_")
    if name in found:
        found[name].append(os.path.relpath(path, mount))
bad = False
for p in programs:
    want = f"programs/{p}/Cargo.toml"
    if found[p] != [want]:
        print(f"::error::library name {p} must resolve to {want} only; found {found[p]}")
        bad = True
if bad:
    sys.exit(1)
print("ok: library names " + ", ".join(f"{p} -> programs/{p}/Cargo.toml" for p in programs) + " (unique)")
PY

commit=$(git rev-parse HEAD)
{
  if [[ -n "${PR_HEAD_SHA:-}" && "$PR_HEAD_SHA" != "$commit" ]]; then
    # actions/checkout on pull_request: refs/pull/N/merge, a synthetic commit on no branch.
    echo "commit $commit (pull-request merge commit, on no branch: PR head $PR_HEAD_SHA merged into its base)"
  else
    echo "commit $commit"
  fi
  echo "solana-verify $SOLANA_VERIFY_VERSION; image $SVB_REPO@$SVB_IMAGE_DIGEST"
  echo "args: --arch $SBPF_ARCH --cargo-build-sbf-args=\"--tools-version $PLATFORM_TOOLS\""
  echo "# program  sha256(file)  executable-hash(trailing zeros trimmed; on-chain comparison)  bytes"
} > "$out/verifiable-hashes.txt"
fail=0
for p in "${PROGRAMS[@]}"; do
  echo "::group::solana-verify build --library-name $p"
  log="$out/$p.build.log"
  solana-verify build --library-name "$p" --arch "$SBPF_ARCH" \
    --cargo-build-sbf-args="--tools-version $PLATFORM_TOOLS" 2>&1 | tee "$log"
  echo "::endgroup::"
  grep -q "Found docker image for Solana version 4.1.2" "$log" || { echo "::error::$p: not built in the 4.1.2 image"; exit 1; }
  [[ $local_mode == 1 ]] || sudo chown -R "$(id -u):$(id -g)" target
  so="target/deploy/$p.so"
  flags=$(od -An -t x4 -j 48 -N 4 "$so" | tr -d ' ')
  [[ "$flags" == 00000003 ]] || { echo "::error::$so e_flags=$flags, expected SBPF v3"; exit 1; }
  printed=$(grep -E '^[0-9a-f]{64}$' "$log" | tail -1)
  exe=$(solana-verify get-executable-hash "$so")
  [[ "$printed" == "$exe" ]] || { echo "::error::$p: printed hash $printed != $exe"; exit 1; }
  cp "$so" "$out/$p.so"
  echo "$p $(sha256sum "$so" | cut -c1-64) $exe $(stat -c %s "$so")" >> "$out/verifiable-hashes.txt"
  if [[ -n "$compare" ]]; then
    if cmp -s "$so" "$compare/$p.so"; then
      echo "ok: $p is byte-identical to $compare/$p.so"
    else
      echo "::error::$p differs from $compare/$p.so ($(sha256sum < "$compare/$p.so" | cut -c1-64))"; fail=1
    fi
  fi
done
cat "$out/verifiable-hashes.txt"
exit $fail
