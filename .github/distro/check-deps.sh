#!/bin/sh
# Runs `krypt deps` against the real package manager of the distro container
# it is in: manager detection, `--check` accepting a real package and rejecting
# a missing one, an install as root with no sudo, and an idempotent re-run.
#
# Usage: check-deps.sh <path-to-krypt> <expected-manager>
set -eu

krypt=$1
manager=$2
config=.github/distro/deps.toml

fail() {
    echo "::error::$*"
    exit 1
}

out=$("$krypt" deps --config "$config" --group present --check) ||
    fail "deps --check rejected the present group"
echo "$out"
[ "$(echo "$out" | head -n 1)" = "manager: $manager (check)" ] ||
    fail "expected $manager to be detected"

if "$krypt" deps --config "$config" --group absent --check; then
    fail "deps --check accepted a package that does not exist"
fi

if command -v jq >/dev/null; then
    fail "jq is preinstalled, so installing it proves nothing"
fi
"$krypt" deps --config "$config" --group present
command -v jq >/dev/null || fail "jq is not on PATH after krypt deps"

out=$("$krypt" deps --config "$config" --group present)
echo "$out"
echo "$out" | grep -qx "already installed: jq" ||
    fail "a second krypt deps run did not find jq installed"

echo "krypt deps verified with $manager"
