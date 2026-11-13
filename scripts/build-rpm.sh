#!/usr/bin/env bash
# Builds the Tiro RPM with a new version: the patch number goes up by one on every build, and you are asked
# whether to raise the minor number instead (which resets patch to 0). If the build fails, the version is put back.
set -euo pipefail

cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

current=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml)
IFS=. read -r major minor patch <<<"$current"

answer=n
if [ -t 0 ]; then
  read -r -p "Version is $current. Raise the minor number to $major.$((minor + 1)).0? [y/N] " answer
fi
case "$answer" in
  [yY]*) next="$major.$((minor + 1)).0" ;;
  *) next="$major.$minor.$((patch + 1))" ;;
esac

files=(Cargo.toml Cargo.lock package.json package-lock.json)
backup=$(mktemp -d)
cp "${files[@]}" "$backup"
restore() {
  cp "${files[@]/#/$backup/}" .
  echo "Build failed; version left at $current." >&2
}
trap restore ERR

sed -i "s/^version = \"$current\"$/version = \"$next\"/" Cargo.toml
npm version "$next" --no-git-tag-version --allow-same-version >/dev/null
echo "Building Tiro $next…"

npm run tauri build -- --bundles rpm

trap - ERR
rm -rf "$backup"
echo
echo "Built version $next. Install it with:"
echo "  sudo dnf install ./target/release/bundle/rpm/Tiro-$next-1.x86_64.rpm"
