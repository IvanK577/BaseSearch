#!/usr/bin/env sh
# Base Search - guided first-run setup and launcher for macOS and Linux.
#
# Unlike run.sh (a quiet build-and-run helper), this script narrates every
# step in the terminal so a non-technical user can watch what is happening:
#
#   1. Detect the operating system.
#   2. Make sure the build tools are present - install the Rust toolchain and,
#      on Linux, the GUI libraries, only when they are missing.
#   3. Install and build the browser workspace from its lockfile.
#   4. Build Base Search. The first build downloads dependencies and takes a
#      few minutes; later runs reuse the downloaded dependencies.
#   5. Launch the app.
#
# Usage:
#   ./start.sh        Run it once. Re-run anytime to start the app again -
#                     already-finished steps are skipped automatically.
#
# No arguments, no flags. For the command-line tool use ./run.sh cli ... .

set -e
cd "$(dirname "$0")"

# --- Output helpers -------------------------------------------------------
# Emit ANSI colors only when stdout is an interactive terminal and the user
# has not opted out via NO_COLOR (https://no-color.org).
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    BOLD=$(printf '\033[1m')
    DIM=$(printf '\033[2m')
    GREEN=$(printf '\033[32m')
    YELLOW=$(printf '\033[33m')
    RED=$(printf '\033[31m')
    CYAN=$(printf '\033[36m')
    RESET=$(printf '\033[0m')
else
    BOLD= DIM= GREEN= YELLOW= RED= CYAN= RESET=
fi

STEP=0
TOTAL=5

step() {
    STEP=$((STEP + 1))
    printf '\n%s[%s/%s]%s %s%s%s\n' "$CYAN" "$STEP" "$TOTAL" "$RESET" "$BOLD" "$1" "$RESET"
}
# Status glyphs use octal escapes (POSIX printf), so they render under dash
# (Ubuntu's /bin/sh) as well as bash; \xHH hex escapes are not portable.
ok()   { printf '   %s\342\234\223%s %s\n' "$GREEN" "$RESET" "$1"; }
info() { printf '   %s\342\200\242%s %s\n' "$DIM" "$RESET" "$1"; }
warn() { printf '   %s!%s %s\n' "$YELLOW" "$RESET" "$1"; }
fail() { printf '   %s\342\234\227%s %s\n' "$RED" "$RESET" "$1" >&2; }

# --- Banner ---------------------------------------------------------------
printf '\n%s%s==============================================%s\n' "$BOLD" "$CYAN" "$RESET"
printf '%s%s   Base Search - first-run setup%s\n' "$BOLD" "$CYAN" "$RESET"
printf '%s%s==============================================%s\n' "$BOLD" "$CYAN" "$RESET"
printf '%sThis prepares everything once, then opens the app.%s\n' "$DIM" "$RESET"

# --- Step 1: detect the system -------------------------------------------
step "Checking your system"
OS=$(uname -s 2>/dev/null || echo unknown)
case "$OS" in
    Darwin) PLATFORM=macOS ;;
    Linux)  PLATFORM=Linux ;;
    *)      PLATFORM=$OS ;;
esac
info "Operating system: $PLATFORM"
if [ "$PLATFORM" != "macOS" ] && [ "$PLATFORM" != "Linux" ]; then
    fail "This guided script supports macOS and Linux."
    info "On Windows, use a release package or scripts\\package-release.ps1."
    exit 1
fi

# --- Step 2: build prerequisites -----------------------------------------
step "Making sure the build tools are installed"

if [ "$PLATFORM" = "macOS" ]; then
    # The Rust compiler links against Apple's command-line tools (clang, etc.).
    if xcode-select -p >/dev/null 2>&1; then
        ok "Xcode Command Line Tools are installed"
    else
        warn "Xcode Command Line Tools are missing - opening Apple's installer"
        info "A macOS window will appear. Click \"Install\" and wait for it to finish,"
        info "then run ./start.sh again to continue."
        xcode-select --install >/dev/null 2>&1 || true
        exit 1
    fi
fi

if [ "$PLATFORM" = "Linux" ]; then
    # egui needs the X11/Wayland and keyboard libraries at build time.
    MISSING_GUI_LIBS=
    if command -v pkg-config >/dev/null 2>&1; then
        for module in xkbcommon wayland-client xcb xcb-render xcb-shape xcb-xfixes; do
            if ! pkg-config --exists "$module" 2>/dev/null; then
                MISSING_GUI_LIBS="$MISSING_GUI_LIBS $module"
            fi
        done
    else
        MISSING_GUI_LIBS=" pkg-config"
    fi
    if [ -z "$MISSING_GUI_LIBS" ]; then
        ok "GUI build libraries are present"
    else
        warn "Missing GUI build libraries:$MISSING_GUI_LIBS"
        warn "Installing the GUI build libraries (this needs your password for sudo)"
        SUDO=
        if [ "$(id -u)" -ne 0 ]; then SUDO=sudo; fi
        if command -v apt-get >/dev/null 2>&1; then
            $SUDO apt-get update
            $SUDO apt-get install -y build-essential pkg-config libxkbcommon-dev \
                libwayland-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
                curl git
            ok "GUI build libraries installed"
        elif command -v dnf >/dev/null 2>&1; then
            $SUDO dnf install -y gcc pkgconf-pkg-config libxkbcommon-devel \
                wayland-devel libxcb-devel curl git
            ok "GUI build libraries installed"
        elif command -v pacman >/dev/null 2>&1; then
            $SUDO pacman -Sy --needed --noconfirm base-devel libxkbcommon wayland \
                libxcb curl git
            ok "GUI build libraries installed"
        else
            fail "Could not detect your package manager (apt, dnf, or pacman)."
            info "Install these dev packages manually, then re-run: a C compiler,"
            info "pkg-config, libxkbcommon, wayland, and libxcb. See the README."
            exit 1
        fi
    fi
fi

# The Rust toolchain (same path on macOS and Linux).
if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
    . "$HOME/.cargo/env"
fi
if command -v cargo >/dev/null 2>&1; then
    ok "Rust toolchain found ($(cargo --version 2>/dev/null))"
else
    warn "Installing the Rust toolchain from https://rustup.rs"
    if curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y; then
        . "$HOME/.cargo/env"
        ok "Rust installed ($(cargo --version 2>/dev/null))"
    else
        fail "Could not install Rust automatically."
        info "Install it from https://rustup.rs and run ./start.sh again."
        exit 1
    fi
fi

# The browser workspace is embedded into the Rust binary, so Node/npm are build
# prerequisites rather than optional developer tooling.
if command -v node >/dev/null 2>&1 && command -v npm >/dev/null 2>&1; then
    ok "Node.js and npm found ($(node --version 2>/dev/null), npm $(npm --version 2>/dev/null))"
else
    fail "Node.js and npm are required to build the browser workspace."
    info "Install the current Node.js LTS release from https://nodejs.org and run ./start.sh again."
    exit 1
fi

# --- Step 3: browser workspace -------------------------------------------
step "Building the browser workspace"
info "Installing the exact frontend dependencies from web-ui/package-lock.json."
if npm --prefix web-ui ci && npm --prefix web-ui run build; then
    if [ ! -f web-ui/dist/index.html ]; then
        fail "The frontend build finished without creating web-ui/dist/index.html."
        exit 1
    fi
    ok "Browser workspace is ready for embedding"
else
    fail "The browser workspace build failed. The npm messages above explain why."
    exit 1
fi

# --- Step 4: build --------------------------------------------------------
step "Building Base Search"
info "The first build downloads dependencies and can take a few minutes."
info "Later runs are instant when nothing changed. Progress is shown below:"
printf '\n'
BUILD_START=$(date +%s 2>/dev/null || echo 0)
if cargo build --release; then
    NOW=$(date +%s 2>/dev/null || echo 0)
    if [ "$BUILD_START" -gt 0 ] && [ "$NOW" -ge "$BUILD_START" ]; then
        printf '\n'
        ok "Build finished in $((NOW - BUILD_START))s"
    else
        printf '\n'
        ok "Build finished"
    fi
else
    fail "The build failed. The compiler messages above explain why."
    info "Most common causes: no internet on the first build, or missing system"
    info "libraries. See the Build From Source section in the README."
    exit 1
fi

# --- Step 5: launch -------------------------------------------------------
step "Launching Base Search"
ok "Starting the app. You can keep this terminal open or minimize it."
printf '\n'
exec ./target/release/BaseSearch
