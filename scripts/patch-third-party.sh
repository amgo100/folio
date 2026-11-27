#!/usr/bin/env bash
# The patched copies of upstream crates in third_party/ (SPEC §2.4,
# D-300-1): crates whose floating-point code calls the platform's maths
# library, changed to call the `libm` crate instead, so that SVG figures
# give the same PDF bytes on every platform. third_party/README.md
# describes them.
#
# Usage: scripts/patch-third-party.sh [--check | --diff <crate>]
#   (none)          rebuild every copy: download the upstream .crate from
#                   crates.io, check its SHA-256 against
#                   third_party/crates.txt, unpack it and apply
#                   third_party/patches/<crate>.patch
#   --check         rebuild every copy in a temporary directory and fail if
#                   one differs from third_party/<crate>
#   --diff <crate>  write third_party/patches/<crate>.patch from the
#                   difference between upstream and third_party/<crate>,
#                   after editing the copy
#
# To bump a crate: change its version and checksum in crates.txt (the
# checksum is the one Cargo.lock records for the crates.io release), run
# the script with no arguments, fix the hunks that no longer apply in the
# copy by hand, write the patch again with --diff, and run
# `cargo test -p folio-pdf --test third_party`.
set -euo pipefail
cd "$(dirname "$0")/.."

mode=apply
only=
case "${1:-}" in
    "") ;;
    --check) mode=check ;;
    --diff)
        mode=diff
        only=${2:?usage: scripts/patch-third-party.sh --diff <crate>}
        ;;
    *)
        echo "usage: scripts/patch-third-party.sh [--check | --diff <crate>]" >&2
        exit 2
        ;;
esac

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Unpacks the upstream release of a crate into $tmp/upstream/<crate>.
upstream() {
    local name=$1 version=$2 sum=$3
    local file="$tmp/$name-$version.crate"
    curl --fail --silent --show-error --location --retry 3 \
        --output "$file" "https://static.crates.io/crates/$name/$name-$version.crate"
    if [[ "$(sha256sum "$file" | cut -d' ' -f1)" != "$sum" ]]; then
        echo "error: $name $version does not match its checksum in third_party/crates.txt" >&2
        exit 1
    fi
    mkdir -p "$tmp/upstream"
    tar -xzf "$file" -C "$tmp/upstream"
    mv "$tmp/upstream/$name-$version" "$tmp/upstream/$name"
}

failed=0
while read -r name version sum; do
    [[ -z "$name" || "$name" == \#* ]] && continue
    [[ -n "$only" && "$name" != "$only" ]] && continue
    upstream "$name" "$version" "$sum"
    patch_file="third_party/patches/$name.patch"
    case "$mode" in
        diff)
            mv "$tmp/upstream/$name" "$tmp/a"
            cp -R "third_party/$name" "$tmp/b"
            # diff exits with 1 when the trees differ, as they should.
            status=0
            (cd "$tmp" && diff -ruN a b) >"$patch_file" || status=$?
            if ((status > 1)); then
                echo "error: diff failed for $name" >&2
                exit 1
            fi
            rm -rf "$tmp/a" "$tmp/b"
            echo "wrote $patch_file"
            ;;
        apply | check)
            out="$tmp/patched/$name"
            mkdir -p "$tmp/patched"
            cp -R "$tmp/upstream/$name" "$out"
            patch --quiet --forward --strip=1 --directory="$out" <"$patch_file"
            if [[ "$mode" == check ]]; then
                if ! diff -r "$out" "third_party/$name" >"$tmp/diff.txt"; then
                    echo "error: third_party/$name differs from $name $version with $patch_file:" >&2
                    head -40 "$tmp/diff.txt" >&2
                    failed=1
                else
                    echo "ok   $name $version"
                fi
            else
                rm -rf "third_party/$name"
                cp -R "$out" "third_party/$name"
                echo "wrote third_party/$name ($name $version with $patch_file)"
            fi
            rm -rf "$tmp/upstream/$name"
            ;;
    esac
done <third_party/crates.txt

exit "$failed"
