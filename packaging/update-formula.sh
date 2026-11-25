#!/bin/sh
# Render the Homebrew formula for a released version and push it to the tap.
#   packaging/update-formula.sh 0.1.0
set -eu
v="$1"
here="$(cd "$(dirname "$0")" && pwd)"
asset="styrman-$v-aarch64-apple-darwin.tar.gz"
sha=$(curl -fsSL "https://github.com/christoffermattssonlangseth/styrman/releases/download/v$v/$asset.sha256" | cut -d' ' -f1)
tap=$(mktemp -d)
gh repo clone christoffermattssonlangseth/homebrew-styrman "$tap" -- -q
mkdir -p "$tap/Formula"
sed -e "s/@VERSION@/$v/g" -e "s/@SHA256@/$sha/g" "$here/styrman.rb.in" > "$tap/Formula/styrman.rb"
git -C "$tap" add Formula/styrman.rb
# Commit as the main repo's (local) identity.
name=$(git -C "$here/.." config user.name); email=$(git -C "$here/.." config user.email)
git -C "$tap" -c user.name="$name" -c user.email="$email" commit -qm "styrman $v"
git -C "$tap" push -q
echo "tap updated to $v ($sha)"
