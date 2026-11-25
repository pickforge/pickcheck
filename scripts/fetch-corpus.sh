#!/bin/sh
# Fetch every repository in tests/corpus/manifest.json into <dir>/<name>,
# checked out at its pinned commit. An entry already at its commit is skipped.
set -eu

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <dir>" >&2
    exit 2
fi

manifest="$(cd "$(dirname "$0")/.." && pwd)/tests/corpus/manifest.json"
mkdir -p "$1"
dir="$(cd "$1" && pwd)"
jq -e '.repositories | length > 0' "$manifest" >/dev/null

jq -r '.repositories[] | [.name, .url, .commit] | @tsv' "$manifest" |
    while IFS="$(printf '\t')" read -r name url commit; do
        target="$dir/$name"
        if [ -d "$target/.git" ] &&
            [ "$(git -C "$target" rev-parse HEAD 2>/dev/null)" = "$commit" ] &&
            [ -z "$(git -C "$target" status --porcelain --untracked-files=all)" ]; then
            echo "$name: already at $commit"
            continue
        fi
        echo "$name: fetching $commit"
        rm -rf "$target"
        git init -q "$target"
        git -C "$target" fetch -q --depth 1 "$url" "$commit"
        git -C "$target" -c advice.detachedHead=false checkout -q FETCH_HEAD
        actual="$(git -C "$target" rev-parse HEAD)"
        if [ "$actual" != "$commit" ]; then
            echo "$name: checked out $actual, expected $commit" >&2
            exit 1
        fi
    done
