#!/usr/bin/env bash
# setup-linux.sh — one-time setup for ais-analytics on Linux (Debian/Ubuntu/Fedora/Arch)
#
# Installs: WebKitGTK (runtime), Azure CLI
# Already-installed tools are skipped automatically.
#
# Usage (from the extracted release archive):
#   chmod +x setup-linux.sh && sudo ./setup-linux.sh

set -e

info()  { echo -e "\033[34m[info]\033[0m  $*"; }
ok()    { echo -e "\033[32m[ok]\033[0m    $*"; }
skip()  { echo -e "\033[33m[skip]\033[0m  $*"; }
warn()  { echo -e "\033[33m[warn]\033[0m  $*"; }
err()   { echo -e "\033[31m[error]\033[0m $*"; exit 1; }

# ── Detect distro ─────────────────────────────────────────────────────────────
if   command -v apt-get &>/dev/null; then DISTRO=debian
elif command -v dnf     &>/dev/null; then DISTRO=fedora
elif command -v pacman  &>/dev/null; then DISTRO=arch
else err "Unsupported distro — install dependencies manually (see README)"
fi

info "Detected distro family: $DISTRO"

# ── Runtime dependencies (WebKitGTK + libxdo) ────────────────────────────────
case "$DISTRO" in
  debian)
    PKGS=()
    dpkg -l libwebkit2gtk-4.1-0 &>/dev/null || dpkg -l libwebkit2gtk-4.0-0 &>/dev/null || PKGS+=(libwebkit2gtk-4.1-0)
    dpkg -l libxdo3 &>/dev/null || PKGS+=(libxdo3)
    if [ ${#PKGS[@]} -gt 0 ]; then
      info "Installing runtime libs: ${PKGS[*]}"
      apt-get update -qq
      apt-get install -y "${PKGS[@]}" 2>/dev/null || apt-get install -y libwebkit2gtk-4.0-0 libxdo3
      ok "Runtime libs installed"
    else
      skip "Runtime libs already installed"
    fi
    ;;
  fedora)
    if ! rpm -q webkit2gtk4.1 &>/dev/null && ! rpm -q webkit2gtk3 &>/dev/null; then
      info "Installing WebKitGTK runtime..."
      dnf install -y webkit2gtk4.1 2>/dev/null || dnf install -y webkit2gtk3
      ok "WebKitGTK installed"
    else
      skip "WebKitGTK already installed"
    fi
    rpm -q xdotool &>/dev/null || dnf install -y xdotool
    ;;
  arch)
    if ! pacman -Qi webkit2gtk-4.1 &>/dev/null && ! pacman -Qi webkit2gtk &>/dev/null; then
      info "Installing WebKitGTK runtime..."
      pacman -S --noconfirm webkit2gtk-4.1 2>/dev/null || pacman -S --noconfirm webkit2gtk
      ok "WebKitGTK installed"
    else
      skip "WebKitGTK already installed"
    fi
    pacman -Qi xdotool &>/dev/null || pacman -S --noconfirm xdotool
    ;;
esac

# ── Azure CLI ─────────────────────────────────────────────────────────────────
# ais-analytics has no auth of its own: it reads the `az login` session for both
# the ARM calls and the Log Analytics query API. Without the CLI the app can't sign in.
if ! command -v az &>/dev/null; then
  info "Installing Azure CLI..."
  case "$DISTRO" in
    debian)
      curl -sL https://aka.ms/InstallAzureCLIDeb | bash
      ;;
    fedora)
      rpm --import https://packages.microsoft.com/keys/microsoft.asc
      dnf install -y https://packages.microsoft.com/config/rhel/9.0/packages-microsoft-prod.rpm
      dnf install -y azure-cli
      ;;
    arch)
      if command -v yay &>/dev/null; then
        yay -S --noconfirm azure-cli
      elif command -v paru &>/dev/null; then
        paru -S --noconfirm azure-cli
      else
        warn "Azure CLI not installed — install manually via AUR (yay -S azure-cli) or pip"
      fi
      ;;
  esac
  command -v az &>/dev/null && ok "Azure CLI installed ($(az version --query '"azure-cli"' -o tsv))"
else
  skip "Azure CLI already installed ($(az version --query '"azure-cli"' -o tsv 2>/dev/null || az --version | head -1))"
fi

# ── Desktop shortcut (optional) ───────────────────────────────────────────────
BINARY_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DESKTOP_FILE="/usr/share/applications/ais-analytics.desktop"

# Register the bundled icon with the hicolor theme so launchers pick it up.
# Falls back to the generic terminal glyph if the PNG isn't shipped.
ICON_NAME="utilities-terminal"
if [[ -f "$BINARY_DIR/icon.png" ]]; then
  ICON_DIR="/usr/share/icons/hicolor/256x256/apps"
  install -Dm644 "$BINARY_DIR/icon.png" "$ICON_DIR/ais-analytics.png"
  if command -v gtk-update-icon-cache &>/dev/null; then
    gtk-update-icon-cache -q -t /usr/share/icons/hicolor || true
  fi
  ICON_NAME="ais-analytics"
  ok "Icon installed to $ICON_DIR/ais-analytics.png"
fi

if [[ -f "$BINARY_DIR/ais-analytics" && ! -f "$DESKTOP_FILE" ]]; then
  info "Creating .desktop launcher..."
  cat > "$DESKTOP_FILE" <<EOF
[Desktop Entry]
Name=AIS Analytics
Comment=Follow a correlation key through Azure Log Analytics
Exec=$BINARY_DIR/ais-analytics
Icon=$ICON_NAME
Terminal=false
Type=Application
Categories=Development;
StartupWMClass=ais-analytics
EOF
  if command -v update-desktop-database &>/dev/null; then
    update-desktop-database -q /usr/share/applications || true
  fi
  ok "Desktop shortcut created"
fi

echo ""
echo "Setup complete. Run 'az login', then ./ais-analytics to start the app."
