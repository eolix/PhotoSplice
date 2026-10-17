#!/usr/bin/env bash
# Build and package PhotoSplice for Linux (<arch> is x86_64, aarch64 or riscv64):
#
#   $DIST/photosplice-<version>-linux-<arch>.AppImage  any distro with glibc >= the build host's
#   $DIST/photosplice-<version>-linux-<arch>.deb       Debian, Ubuntu, Mint, Pop!_OS, ...
#   $DIST/photosplice-<version>-linux-<arch>.rpm       Fedora, openSUSE, RHEL, ...
#   $DIST/photosplice-<version>-linux-<arch>.pkg.tar.zst  Arch Linux, Manjaro, EndeavourOS, ...
#   $DIST/photosplice-<version>-linux-<arch>.tar.gz    plain FHS-style tree (bin/, share/)
#
# Usage: packaging/linux/package.sh [--skip-build] [--formats "appimage deb rpm arch tar"]
#
# Needs: cargo; nfpm for deb/rpm/arch (https://nfpm.goreleaser.com). By default AppImage tooling
# is downloaded at verified pins into $CARGO_TARGET_DIR. Build on an old distro (CI: Ubuntu 22.04,
# glibc 2.35) so the binaries run on newer ones. Optional: desktop-file-validate, appstreamcli,
# and zsyncmake (package zsync) for the AppImage's .zsync if appimagetool doesn't write it.
#
# The AppImage carries update information for AppImageUpdate: the newest GitHub release of
# $PHOTOSPLICE_UPDATE_REPO (default: $GITHUB_REPOSITORY in CI, else eolix/photosplice), found
# through the .zsync published beside it. packaging/linux/verify-appimage.sh checks the result.
#
# Cross builds (riscv64, tar.gz only): set CROSS_ARCH (riscv64), CROSS_TARGET (the Rust
# triple), CROSS_COMPILE (binutils prefix, for strip) and optionally EMULATOR (e.g. qemu-riscv64)
# to smoke-test the binary.
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
HERE="$ROOT/packaging/linux"
# shellcheck source=verified-download.sh
. "$HERE/verified-download.sh"
APP_ID=io.github.eolix.PhotoSplice

SKIP_BUILD=0
FORMATS="appimage deb rpm arch tar"
while [ $# -gt 0 ]; do
  case "$1" in
    --skip-build) SKIP_BUILD=1; shift ;;
    --formats) FORMATS="$2"; shift 2 ;;
    -h | --help) sed -n '2,23p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

ARCH="${CROSS_ARCH:-$(uname -m)}"
case "$ARCH" in
  riscv64) DEB_ARCH=riscv64 ;;
  x86_64) DEB_ARCH=amd64 ;;
  aarch64 | arm64) ARCH=aarch64; DEB_ARCH=arm64 ;;
  *) echo "unsupported architecture $ARCH" >&2; exit 2 ;;
esac
export PHOTOSPLICE_MAINTAINER="${PHOTOSPLICE_MAINTAINER:-PhotoSplice maintainers <photosplice@gmail.com>}"
BASENAME="photosplice-$VERSION-linux-$ARCH"

echo "==> PhotoSplice $VERSION for Linux $ARCH ($FORMATS)"

if [ "$SKIP_BUILD" = 0 ]; then
  if [ -n "${CROSS_TARGET:-}" ]; then
    (cd "$ROOT" && cargo build --release --locked -p photosplice --target "$CROSS_TARGET")
  else
    (cd "$ROOT" && cargo build --release --locked -p photosplice)
  fi
fi
BIN="$CARGO_TARGET_DIR/${CROSS_TARGET:+$CROSS_TARGET/}release"
WORK="$CARGO_TARGET_DIR/linux-package"
STAGE="$WORK/root"
rm -rf "$WORK"

# ---- stage an FHS tree (shared by every format) -------------------------------------------------
install -Dm755 "$BIN/photosplice" "$STAGE/usr/bin/photosplice"
copy_resources "$STAGE/usr/share/photosplice/resources"
"${CROSS_COMPILE:-}strip" "$STAGE/usr/bin/photosplice" 2>/dev/null || true
install -Dm644 "$HERE/$APP_ID.desktop" "$STAGE/usr/share/applications/$APP_ID.desktop"
install -Dm644 "$HERE/$APP_ID.mime.xml" "$STAGE/usr/share/mime/packages/$APP_ID.xml"
mkdir -p "$STAGE/usr/share/metainfo"
sed -e "s/@VERSION@/$VERSION/g" -e "s/@DATE@/$PHOTOSPLICE_BUILD_DATE/g" \
  "$HERE/$APP_ID.metainfo.xml.in" >"$STAGE/usr/share/metainfo/$APP_ID.metainfo.xml"
mkdir -p "$STAGE/usr/share/icons"
cp -R "$ROOT/assets/app-icon/hicolor" "$STAGE/usr/share/icons/"
mkdir -p "$STAGE/usr/share/doc/photosplice"
copy_docs "$STAGE/usr/share/doc/photosplice"

# Lints: reported, but a style warning in the metadata doesn't stop a release.
if command -v desktop-file-validate >/dev/null; then
  desktop-file-validate "$STAGE/usr/share/applications/$APP_ID.desktop" || warn "desktop file has validation issues"
fi
if command -v appstreamcli >/dev/null; then
  appstreamcli validate --no-net --explain "$STAGE/usr/share/metainfo/$APP_ID.metainfo.xml" || warn "AppStream metadata has validation issues"
fi

has() { case " $FORMATS " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }

# ---- .tar.gz ------------------------------------------------------------------------------------
if has tar; then
  mkdir -p "$WORK/tar"
  cp -R "$STAGE/usr" "$WORK/tar/$BASENAME"
  tar -C "$WORK/tar" -czf "$DIST/$BASENAME.tar.gz" "$BASENAME"
  echo "wrote $DIST/$BASENAME.tar.gz"
fi

# ---- .deb / .rpm / Arch -------------------------------------------------------------------------
if has deb || has rpm || has arch; then
  command -v nfpm >/dev/null || { echo "error: nfpm not found (https://nfpm.goreleaser.com/install/)" >&2; exit 1; }
  export VERSION
  export NFPM_ARCH="$DEB_ARCH"
  # nfpm expands env vars in fields like `version` and `arch`, but not in `contents[].src` or
  # `archlinux.packager`.
  sed -e "s|\${STAGE}|$STAGE|g" -e "s|\${PHOTOSPLICE_MAINTAINER}|$PHOTOSPLICE_MAINTAINER|g" "$HERE/nfpm.yaml" >"$WORK/nfpm.yaml"
  for fmt in deb rpm; do
    if has "$fmt"; then (cd "$ROOT" && nfpm package -f "$WORK/nfpm.yaml" -p "$fmt" -t "$DIST/$BASENAME.$fmt"); fi
  done
  if has arch; then
    # pkgver can't hold a hyphen, and nfpm's semver parsing would drop the pre-release from it:
    # 0.2.0-rc.1 goes in as 0.2.0_rc.1, which pacman sorts before 0.2.0.
    sed "s|^version_schema: semver|version_schema: none|" "$WORK/nfpm.yaml" >"$WORK/nfpm-arch.yaml"
    (cd "$ROOT" && VERSION="${VERSION//-/_}" nfpm package -f "$WORK/nfpm-arch.yaml" -p archlinux -t "$DIST/$BASENAME.pkg.tar.zst")
  fi
fi

# ---- AppImage -----------------------------------------------------------------------------------
if has appimage; then
  APPDIR="$WORK/PhotoSplice.AppDir"
  # The whole staged tree, licences and notices included (NOTICE must travel with the binary).
  cp -R "$STAGE" "$APPDIR"
  # AppRun is a script, not a symlink: it installs the .desktop entry and icons into
  # $XDG_DATA_HOME before launching, so Wayland compositors can resolve the window's
  # app_id to our icon instead of the generic Wayland logo (#593; opt-out env in the
  # script header). Logic is covered by packaging/linux/apprun-test.sh.
  install -Dm755 "$HERE/AppRun" "$APPDIR/AppRun"
  cp "$HERE/$APP_ID.desktop" "$APPDIR/$APP_ID.desktop"
  cp "$ROOT/assets/app-icon/hicolor/256x256/apps/$APP_ID.png" "$APPDIR/$APP_ID.png"
  ln -s "$APP_ID.png" "$APPDIR/.DirIcon"

  # Digests from the official GitHub release asset metadata:
  # https://github.com/AppImage/appimagetool/releases/tag/1.9.1
  # https://github.com/AppImage/type2-runtime/releases/tag/20251108
  # Keep both pins together:
  # appimagetool otherwise downloads the mutable latest type2 runtime itself.
  TOOL_VERSION=1.9.1
  RUNTIME_VERSION=20251108
  case "$ARCH" in
    x86_64)
      TOOL_SHA256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
      RUNTIME_SHA256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d
      ;;
    aarch64)
      TOOL_SHA256=f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158
      RUNTIME_SHA256=00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444
      ;;
  esac
  mkdir -p "$CARGO_TARGET_DIR"
  TOOL="${APPIMAGETOOL:-$CARGO_TARGET_DIR/appimagetool-$TOOL_VERSION-$ARCH.AppImage}"
  if [ -n "${APPIMAGETOOL:-}" ]; then
    verify_appimagetool_override "$TOOL" "$TOOL_SHA256" "$TOOL_VERSION" "$ARCH"
  else
    download_verified \
      "https://github.com/AppImage/appimagetool/releases/download/$TOOL_VERSION/appimagetool-$ARCH.AppImage" \
      "$TOOL" "$TOOL_SHA256" true
  fi
  RUNTIME="$CARGO_TARGET_DIR/type2-runtime-$RUNTIME_VERSION-$ARCH"
  download_verified \
    "https://github.com/AppImage/type2-runtime/releases/download/$RUNTIME_VERSION/runtime-$ARCH" \
    "$RUNTIME" "$RUNTIME_SHA256"
  # Absolute paths: appimagetool runs from $DIST, where it writes the .zsync.
  mkdir -p "$DIST"
  abs() { echo "$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"; }
  TOOL="$(abs "$TOOL")"
  APPDIR="$(abs "$APPDIR")"
  RUNTIME="$(abs "$RUNTIME")"
  OUT="$(abs "$DIST/$BASENAME.AppImage")"
  # Delta updates: `gh-releases-zsync` resolves the newest release through the GitHub API, so
  # the channel doesn't go stale when the version in the file name changes. The file name is a
  # glob for that reason, and must match the asset each release publishes. `latest` is the
  # newest published release that is not a pre-release.
  UPDATE_REPO="${PHOTOSPLICE_UPDATE_REPO:-${GITHUB_REPOSITORY:-eolix/photosplice}}"
  UPDATE_INFO="gh-releases-zsync|${UPDATE_REPO%%/*}|${UPDATE_REPO#*/}|latest|photosplice-*-linux-$ARCH.AppImage.zsync"
  # A .zsync left by an earlier run would hide a missing zsyncmake and describe another file.
  rm -f "$OUT" "$OUT.zsync"
  # Extract-and-run: works without FUSE (containers, CI). The output embeds the static runtime
  # (pinned above: appimagetool otherwise downloads the mutable latest one), so users don't need
  # libfuse2 either. `-u` embeds the update information and writes the .zsync control file
  # beside the AppImage.
  (cd "$DIST" && ARCH="$ARCH" APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream \
    --runtime-file "$RUNTIME" -u "$UPDATE_INFO" "$APPDIR" "$OUT")
  if [ ! -f "$OUT.zsync" ]; then
    if command -v zsyncmake >/dev/null; then
      # The URL is relative: AppImageUpdate fetches the AppImage from beside the .zsync.
      (cd "$DIST" && zsyncmake -u "$(basename "$OUT")" -o "$(basename "$OUT").zsync" "$(basename "$OUT")")
    else
      warn "no .zsync written (install zsync): AppImageUpdate can't update this AppImage"
    fi
  fi
  echo "wrote $OUT"
fi

if [ -z "${CROSS_TARGET:-}" ]; then
  "$STAGE/usr/bin/photosplice" --version
elif [ -n "${EMULATOR:-}" ]; then
  "$EMULATOR" "$STAGE/usr/bin/photosplice" --version
fi
echo "==> done"
ls -lh "$DIST"
