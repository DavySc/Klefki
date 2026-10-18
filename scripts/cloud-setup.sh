#!/usr/bin/env bash
# Prepares a Claude Code cloud session (Ubuntu, root) for CLAUDE.md's checks: the latest stable toolchain (CI
# uses it too), cargo-deny, and the libraries a real window needs under Xvfb. Skips what is already there.
# Run by the SessionStart hook in .claude/settings.json; can also be the cloud environment's setup script, whose
# result is cached between sessions.
set -uo pipefail

DENY_VERSION=0.20.2
DENY_SHA256=9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f
PACKAGES=(xvfb libxkbcommon-x11-0 libegl1 libgl1-mesa-dri xdotool)

failed=()

# The toolchain: `stable` in rust-toolchain.toml means whatever stable is installed; CI's clippy is the newest.
rustup update stable --no-self-update >/dev/null 2>&1 || failed+=("rustup update stable")

# cargo-deny: the release binary (checksum pinned), not `cargo install`, which takes minutes.
bin="${CARGO_HOME:-$HOME/.cargo}/bin"
if [ "$(cargo-deny --version 2>/dev/null)" != "cargo-deny $DENY_VERSION" ]; then
    tmp=$(mktemp -d)
    name="cargo-deny-$DENY_VERSION-x86_64-unknown-linux-musl"
    if curl -sSfL -o "$tmp/deny.tgz" \
            "https://github.com/EmbarkStudios/cargo-deny/releases/download/$DENY_VERSION/$name.tar.gz" \
        && echo "$DENY_SHA256  $tmp/deny.tgz" | sha256sum --check --quiet \
        && tar -xzf "$tmp/deny.tgz" -C "$tmp" \
        && install -D -m 755 "$tmp/$name/cargo-deny" "$bin/cargo-deny"; then
        :
    else
        failed+=("cargo-deny $DENY_VERSION")
    fi
    rm -rf "$tmp"
fi

# Libraries for running the app in a real window under Xvfb (screenshots).
missing=()
for p in "${PACKAGES[@]}"; do
    dpkg -s "$p" >/dev/null 2>&1 || missing+=("$p")
done
if [ ${#missing[@]} -gt 0 ]; then
    { apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "${missing[@]}"; } >/dev/null 2>&1 \
        || failed+=("apt packages: ${missing[*]}")
fi

if [ ${#failed[@]} -gt 0 ]; then
    echo "cloud-setup: could not install: ${failed[*]}. Checks that need them are unverified until fixed."
else
    echo "cloud-setup: $(rustc --version), $(cargo-deny --version), window libraries present."
fi
exit 0
