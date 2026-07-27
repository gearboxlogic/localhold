#!/usr/bin/env bash
set -euo pipefail

repository_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)
check="$repository_root/script/check-maintainability-bootstrap.sh"
fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT
mkdir -p "$fixture/tools/maintainability"

write_manifest() {
    printf '%s\n' "$1" >"$fixture/tools/maintainability/Cargo.toml"
}

expect_failure() {
    if "$check" "$fixture" >/dev/null 2>&1; then
        printf 'maintainability bootstrap fixture unexpectedly passed\n' >&2
        exit 1
    fi
}

write_manifest $'[package]\nname = "checker"\nbuild = false'
"$check" "$fixture" >/dev/null

touch "$fixture/tools/maintainability/build.rs"
expect_failure
rm "$fixture/tools/maintainability/build.rs"

write_manifest $'[package]\nname = "checker"'
expect_failure

write_manifest $'[package]\nname = "checker"\nbuild = true'
expect_failure

printf 'maintainability bootstrap tests passed\n'
