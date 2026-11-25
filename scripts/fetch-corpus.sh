#!/usr/bin/env bash
# Fetch every repository in tests/corpus/manifest.json into <dir>/<name>,
# checked out at its pinned commit. A checkout already at its commit with an
# unchanged tree is reused. Git runs without user or system config, hooks,
# filters, templates or submodules, so no code from a repository runs.
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <dir>" >&2
    exit 2
fi

manifest="$(cd "$(dirname "$0")/.." && pwd)/tests/corpus/manifest.json"

# Validate the whole manifest before touching the corpus. One line per entry.
entries="$(jq -er '
    def entry_ok:
        type == "object"
        and (keys == ["commit", "license", "name", "url"])
        and (.name | type == "string" and test("^[A-Za-z0-9_][A-Za-z0-9._-]*$"))
        and (.url | type == "string" and test("^https://[^[:space:]]+$"))
        and (.commit | type == "string" and test("^[0-9a-f]{40}$"))
        and (.license | type == "string" and length > 0);
    if (.repositories | type) != "array" or (.repositories | length) == 0 then
        error("manifest needs a nonempty repositories array")
    elif any(.repositories[]; entry_ok | not) then
        error("manifest has an invalid entry")
    elif (.repositories | map(.name) | unique | length) != (.repositories | length) then
        error("manifest has duplicate names")
    else
        .repositories[] | [.name, .url, .commit] | @tsv
    end
' "$manifest")"

# Git environment: no inherited repository, config, hooks, filters or prompts.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY \
    GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_CONFIG GIT_CONFIG_PARAMETERS \
    GIT_CONFIG_COUNT GIT_TEMPLATE_DIR GIT_EXEC_PATH
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_ATTR_NOSYSTEM=1 \
    GIT_TERMINAL_PROMPT=0 GIT_LFS_SKIP_SMUDGE=1 GIT_ADVICE=0

safe_git() {
    git -c core.hooksPath=/dev/null -c core.fsmonitor=false \
        -c core.attributesFile=/dev/null -c core.excludesFile=/dev/null \
        -c submodule.recurse=false -c protocol.allow=never \
        -c protocol.https.allow=always "$@"
}

# The only keys `git init` and this script write. Any other local key, such as
# a filter, hook path or include, makes the checkout untrusted.
allowed_config='^(core\.(repositoryformatversion|filemode|bare|logallrefupdates|ignorecase|precomposeunicode|symlinks)|remote\.origin\.(url|fetch))$'

# True when $1 is a checkout this script made from $2.
owned() {
    local target="$1" url="$2"
    [ -d "$target/.git" ] && [ ! -L "$target" ] && [ -f "$target/.git/config" ] &&
        [ "$(git config --file "$target/.git/config" --get remote.origin.url 2>/dev/null)" = "$url" ]
}

# True when $1 is at commit $2 with exactly the tree of that commit.
current() {
    local target="$1" commit="$2" keys head flags status
    keys="$(git config --file "$target/.git/config" --name-only --list)" || return 1
    ! grep -Eqv "$allowed_config" <<<"$keys" || return 1
    if [ -f "$target/.git/info/exclude" ] &&
        grep -Eqv '^[[:space:]]*(#|$)' "$target/.git/info/exclude"; then
        return 1
    fi
    [ ! -e "$target/.git/info/sparse-checkout" ] || return 1
    head="$(safe_git -C "$target" rev-parse --verify -q HEAD)" || return 1
    [ "$head" = "$commit" ] || return 1
    # Skip-worktree and assume-unchanged entries hide missing files from status.
    flags="$(safe_git -C "$target" ls-files -v)" || return 1
    ! grep -qv -e '^H ' -e '^$' <<<"$flags" || return 1
    status="$(safe_git -C "$target" status --porcelain --untracked-files=all)" || return 1
    [ -z "$status" ]
}

mkdir -p "$1"
dir="$(cd "$1" && pwd)"

while IFS=$'\t' read -r name url commit; do
    target="$dir/$name"
    if [ -e "$target" ] || [ -L "$target" ]; then
        if ! owned "$target" "$url"; then
            echo "$name: $target exists but is not a checkout of $url; move it away" >&2
            exit 1
        fi
        if current "$target" "$commit"; then
            echo "$name: already at $commit"
            continue
        fi
        echo "$name: $target is not a clean checkout of $commit; fetching again"
        rm -rf "$target"
    fi
    echo "$name: fetching $commit"
    safe_git init -q --template= "$target"
    safe_git -C "$target" remote add origin "$url"
    safe_git -C "$target" fetch -q --depth 1 --no-tags --no-recurse-submodules origin "$commit"
    safe_git -C "$target" -c advice.detachedHead=false checkout -q FETCH_HEAD
    actual="$(safe_git -C "$target" rev-parse HEAD)"
    if [ "$actual" != "$commit" ]; then
        echo "$name: checked out $actual, expected $commit" >&2
        exit 1
    fi
done <<<"$entries"
