# Releasing

Requires [rust-release-tools](https://github.com/raine/rust-release-tools):

```bash
pipx install git+https://github.com/raine/rust-release-tools.git
```

To release:

```bash
just release-patch  # or release-minor, release-major
```

This will:

1. Bump version in Cargo.toml
2. Generate changelog entry using Claude
3. Open editor to review changelog
4. Commit, publish to crates.io, tag, and push

## Updating flake.nix

After a release, update the Nix flake to match the new version:

```bash
./scripts/update-flake.sh
```

This will:

1. Read the version from Cargo.toml and update flake.nix
2. Recalculate the `cargoHash` for the new dependencies
3. Update `flake.lock`
4. Verify the build and binary
5. Stage the changes for commit

## Backfilling changelog

To generate changelog entries for all git tags missing from CHANGELOG.md:

```bash
update-changelog
```

This uses `cc-batch` to process multiple tags in parallel.
