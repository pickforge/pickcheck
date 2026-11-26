#!/usr/bin/env bash
# Fetch every repository in tests/corpus/manifest.json into <dir>/<name>,
# checked out at its pinned commit. A checkout already at its commit with an
# unchanged tree is reused. Git runs without user or system config, hooks,
# filters, templates or submodules, so no code from a repository runs. The
# script changes or removes only a checkout that it created and marked.
set -euo pipefail

# With CDPATH set, `cd` prints the directory, which would end up in a path.
unset CDPATH

# Git environment: no inherited repository, index, object store, namespace,
# config, hooks, filters or prompts. This comes before any Git command.
unset GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR GIT_INDEX_FILE GIT_OBJECT_DIRECTORY \
    GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_NAMESPACE GIT_CEILING_DIRECTORIES \
    GIT_CONFIG GIT_CONFIG_COUNT GIT_CONFIG_PARAMETERS GIT_TEMPLATE_DIR GIT_EXEC_PATH
for variable in $(compgen -e); do
    case "$variable" in
    GIT_CONFIG_KEY_* | GIT_CONFIG_VALUE_*) unset "$variable" ;;
    esac
done
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_GLOBAL=/dev/null \
    GIT_ATTR_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0 GIT_LFS_SKIP_SMUDGE=1 GIT_ADVICE=0

# Written inside .git when this script creates a checkout.
marker=pickcheck-corpus-checkout

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

safe_git() {
    git -c core.hooksPath=/dev/null -c core.fsmonitor=false \
        -c core.attributesFile=/dev/null -c core.excludesFile=/dev/null \
        -c submodule.recurse=false -c protocol.allow=never \
        -c protocol.https.allow=always "$@"
}

# The only keys `git init` and this script write. Any other local key, such as
# a filter, hook path or include, makes the checkout untrusted.
allowed_config='^(core\.(repositoryformatversion|filemode|bare|logallrefupdates|ignorecase|precomposeunicode|symlinks)|remote\.origin\.(url|fetch))$'

# True when $1 is a checkout that this script created, proved by its marker.
owned() {
    local target="$1"
    [ ! -L "$target" ] && [ -d "$target/.git" ] && [ ! -L "$target/.git" ] &&
        [ -f "$target/.git/$marker" ] && [ ! -L "$target/.git/$marker" ]
}

# True when $1 is a checkout of $2 at commit $3 with exactly the tree of that
# commit.
current() {
    local target="$1" url="$2" commit="$3" keys head flags status stage mode path
    keys="$(git config --file "$target/.git/config" --name-only --list)" || return 1
    [ "$(git config --file "$target/.git/config" --get remote.origin.url)" = "$url" ] ||
        return 1
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
    [ -z "$status" ] || return 1
    # Status does not look inside a submodule directory, but the scan reads it.
    stage="$(safe_git -C "$target" ls-files --stage)" || return 1
    while IFS=$'\t' read -r mode path; do
        case "$mode" in
        "160000 "*) [ -z "$(ls -A "$target/$path" 2>/dev/null)" ] || return 1 ;;
        esac
    done <<<"$stage"
}

mkdir -p "$1"
dir="$(cd "$1" && pwd)"

while IFS=$'\t' read -r name url commit; do
    target="$dir/$name"
    if [ -e "$target" ] || [ -L "$target" ]; then
        if ! owned "$target"; then
            echo "$name: $target was not created by this script; move it away" >&2
            exit 1
        fi
        if current "$target" "$url" "$commit"; then
            echo "$name: already at $commit"
            continue
        fi
        echo "$name: $target is not a clean checkout of $commit; fetching again"
        rm -rf "$target"
    fi
    echo "$name: fetching $commit"
    safe_git init -q --template= "$target"
    : >"$target/.git/$marker"
    safe_git -C "$target" remote add origin "$url"
    safe_git -C "$target" fetch -q --depth 1 --no-tags --no-recurse-submodules origin "$commit"
    safe_git -C "$target" -c advice.detachedHead=false checkout -q FETCH_HEAD
    actual="$(safe_git -C "$target" rev-parse HEAD)"
    if [ "$actual" != "$commit" ]; then
        echo "$name: checked out $actual, expected $commit" >&2
        exit 1
    fi
done <<<"$entries"
