#!/usr/bin/env bash
set -euo pipefail

FLAKE_FILE="flake.nix"

# Get version from Cargo.toml
NEW_VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
OLD_VERSION=$(grep 'version = "' "$FLAKE_FILE" | head -1 | sed 's/.*"\(.*\)".*/\1/')

if [ "$NEW_VERSION" = "$OLD_VERSION" ]; then
  echo "flake.nix already at version $NEW_VERSION"
else
  echo "Updating version: $OLD_VERSION -> $NEW_VERSION"
  sed -i "s/version = \"$OLD_VERSION\"/version = \"$NEW_VERSION\"/" "$FLAKE_FILE"
fi

# Clear cargoHash to trigger recalculation
sed -i 's|cargoHash = "sha256-.*"|cargoHash = ""|' "$FLAKE_FILE"

# Stage so nix can see the changes
git add "$FLAKE_FILE"

# Get the correct hash from the build error
echo "Calculating new cargoHash..."
HASH=$(nix build --no-link 2>&1 | grep "got:" | awk '{print $2}')

if [ -z "$HASH" ]; then
  echo "ERROR: Could not determine cargoHash. Build may have succeeded with empty hash or failed unexpectedly."
  exit 1
fi

echo "New cargoHash: $HASH"
sed -i "s|cargoHash = \"\"|cargoHash = \"$HASH\"|" "$FLAKE_FILE"

# Update flake.lock
nix flake update

# Stage updated files
git add "$FLAKE_FILE" flake.lock

# Verify
echo "Verifying build..."
nix build --no-link
echo "Verifying binary..."
nix run . -- --version

echo ""
echo "flake.nix updated to version $NEW_VERSION with new hashes."
echo "Changes are staged. Review with 'git diff --cached' and commit when ready."
