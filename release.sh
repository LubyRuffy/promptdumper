#!/usr/bin/env bash
set -euo pipefail

# 1. 自动提取版本号
NAME=$(grep '^name' src-tauri/Cargo.toml | head -n1 | sed -E 's/name *= *"([^"]+)"/\1/')
VERSION=$(grep '^version' src-tauri/Cargo.toml | head -n1 | sed -E 's/version *= *"([^"]+)"/\1/')
TAG="v$VERSION"

echo "Building and releasing version $TAG"

# 2. 构建 Tauri
cd src-tauri
cargo tauri build #mac, app+dmg
cargo tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc --bundles app #windows, app
# cargo tauri build --target x86_64-pc-windows-gun --no-bundle #windows, app
cd ..

zip -9 -r src-tauri/target/x86_64-pc-windows-msvc/release/"$NAME"_"$VERSION"_windows_x64.zip src-tauri/target/x86_64-pc-windows-msvc/release/$NAME.exe

# 3. 打 tag 并推送
git tag "$TAG" || true
git push origin "$TAG"

# 4. 创建 Release 并上传产物
gh release view "$TAG" >/dev/null 2>&1 || gh release create "$TAG" --notes "Local build $TAG"
gh release upload "$TAG" \
  src-tauri/target/release/bundle/dmg/"$NAME"_"$VERSION"_aarch64.dmg \
  src-tauri/target/x86_64-pc-windows-msvc/release/"$NAME"_"$VERSION"_windows_x64.zip \
  --clobber
