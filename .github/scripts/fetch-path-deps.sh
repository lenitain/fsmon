#!/usr/bin/env bash
# Materialize the sibling path dependencies declared in Cargo.toml.
#
# CI has no sibling checkouts. Prefer the published crate so the build uses
# exactly what crates.io serves; when a release is not published yet, fall back
# to the matching git tag (or main, which must still carry that version).
set -euo pipefail

while read -r name version dir; do
    dest="../$dir"
    repo="https://github.com/lenitain/$name.git"

    if [ -d "$dest" ]; then
        echo "Using existing $dest" >&2
        continue
    fi

    if curl -fsSL "https://crates.io/api/v1/crates/$name/$version/download" \
        -o "/tmp/$name.crate"; then
        mkdir -p "$dest"
        tar -xzf "/tmp/$name.crate" -C "$dest" --strip-components=1
        continue
    fi

    echo "$name $version is not published yet, cloning $repo" >&2
    if git ls-remote --exit-code --tags "$repo" "refs/tags/$version" >/dev/null 2>&1; then
        git clone --depth 1 --branch "$version" "$repo" "$dest"
    elif git ls-remote --exit-code --tags "$repo" "refs/tags/v$version" >/dev/null 2>&1; then
        git clone --depth 1 --branch "v$version" "$repo" "$dest"
    else
        git clone --depth 1 "$repo" "$dest"
    fi

    actual=$(grep -m1 '^version = ' "$dest/Cargo.toml" | cut -d'"' -f2)
    if [ "$actual" != "$version" ]; then
        echo "::error::$name at $dest is $actual, Cargo.toml requires $version" >&2
        exit 1
    fi
done < <(sed -n 's/^\([A-Za-z0-9_-]*\) = { version = "\([^"]*\)".*path = "\.\.\/\([^"]*\)".*/\1 \2 \3/p' Cargo.toml)
