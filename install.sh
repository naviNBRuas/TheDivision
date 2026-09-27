#!/bin/sh
# divisi installer.
#
#   curl -fsSL https://raw.githubusercontent.com/naviNBRuas/TheDivision/main/install.sh | sh
#
# Downloads the prebuilt divisi binaries (`divisi` CLI/TUI, `divisid` runtime daemon, `divisi-mcp`,
# `divisi-gateway`, `divisi-agent`, `divisi-lsp`, `divisi-notch`) for your platform from the latest
# GitHub release and installs them to $DIVISI_INSTALL_DIR (default: ~/.local/bin).
#
# Supported platforms: Linux x86_64/arm64, macOS x86_64/arm64 (Intel and
# Apple Silicon). Anything else: build from source with
# `cargo build --release --workspace` instead (see README.md).

set -eu

# The repository keeps its pre-rename name until it is renamed on GitHub (which redirects).
REPO="naviNBRuas/TheDivision"
INSTALL_DIR="${DIVISI_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${DIVISI_VERSION:-latest}"

info() { printf '>> %s\n' "$1"; }
error() { printf 'error: %s\n' "$1" >&2; exit 1; }

detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Linux) os_name="linux" ;;
    Darwin) os_name="macos" ;;
    *) error "unsupported OS: $os (build from source instead, see README.md)" ;;
  esac

  case "$arch" in
    x86_64 | amd64) arch_name="x86_64" ;;
    arm64 | aarch64) arch_name="arm64" ;;
    *) error "unsupported architecture: $arch (build from source instead, see README.md)" ;;
  esac

  printf '%s-%s' "$os_name" "$arch_name"
}

main() {
  command -v curl >/dev/null 2>&1 || error "curl is required"
  command -v tar >/dev/null 2>&1 || error "tar is required"

  target="$(detect_target)"
  info "Detected platform: $target"

  if [ "$VERSION" = "latest" ]; then
    base="https://github.com/$REPO/releases/latest/download"
  else
    base="https://github.com/$REPO/releases/download/$VERSION"
  fi

  work_dir="$(mktemp -d)"
  trap 'rm -rf "$work_dir"' EXIT

  asset="divisi-$target"
  info "Downloading $base/$asset.tar.gz"
  curl -fsSL "$base/$asset.tar.gz" -o "$work_dir/divisi.tar.gz" \
    || error "download failed — is there a release for $target yet? See https://github.com/$REPO/releases"

  tar -xzf "$work_dir/divisi.tar.gz" -C "$work_dir"

  mkdir -p "$INSTALL_DIR"
  installed=""
  for bin in divisi divisid divisi-mcp divisi-gateway divisi-agent divisi-lsp divisi-notch; do
    if [ -f "$work_dir/$asset/$bin" ]; then
      cp "$work_dir/$asset/$bin" "$INSTALL_DIR/$bin"
      chmod +x "$INSTALL_DIR/$bin"
      installed="$installed $bin"
    fi
  done
  [ -n "$installed" ] || error "the $asset archive contained no divisi binaries"

  info "Installed to $INSTALL_DIR:$installed"

  case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
      info "$INSTALL_DIR is not on your PATH. Add it with:"
      printf '\n'
      printf '  bash/zsh:  echo '\''export PATH="%s:$PATH"'\'' >> ~/.bashrc   # or ~/.zshrc\n' "$INSTALL_DIR"
      printf '  fish:      fish_add_path %s\n' "$INSTALL_DIR"
      printf '\n'
      ;;
  esac

  info "Run 'divisi doctor' to check what divisi can manage on this machine, or just 'divisi' to open the TUI."
}

main
