#!/usr/bin/env bash
# Set up Freekstyle from your own disc dump: put the dump (.bin or .iso) in
# game/ and run ./setup.sh. It installs what is missing (asking first),
# copies what the port needs off the disc, generates the game code and
# builds it. Steps already done are skipped, so it is safe to run again
# (e.g. after recording VU programs, see README).
set -euo pipefail
cd "$(dirname "$0")"

SLUS_SHA256=debcb4b0731741a389a0c890296609eb99d111952e97a250ea4c3cfc04a6d80a
MOVIES="OPENING/BIGLOGO OPENING/EAGAMES OPENING/EAS_BIG OPENING/INTRO ABOUTEA/PROD1"

step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
fail() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }
ask() {
    local reply
    read -r -p "$1 [Y/n] " reply || reply=n
    [[ -z $reply || $reply == [Yy]* ]]
}
have() { command -v "$1" >/dev/null 2>&1; }

# ---- 1. The disc dump ----
step "Looking for your disc dump in game/"
mkdir -p game
if [[ ! -e game/Freekstyle.bin ]]; then
    shopt -s nullglob nocaseglob
    dumps=(game/*.bin game/*.iso)
    shopt -u nullglob nocaseglob
    case ${#dumps[@]} in
        0) fail "no disc image in game/. Dump your Freekstyle disc (NTSC-U, SLUS-20367) as a .bin or .iso into game/, then run this again." ;;
        1) ln -s "$(basename "${dumps[0]}")" game/Freekstyle.bin
           echo "using ${dumps[0]} (linked as game/Freekstyle.bin)" ;;
        *) fail "several disc images in game/ (${dumps[*]}); rename yours to Freekstyle.bin." ;;
    esac
else
    echo "found game/Freekstyle.bin"
fi

# ---- 2. Tools and libraries ----
step "Checking tools and libraries"
pm=""
for p in pacman apt-get dnf zypper; do
    if have "$p"; then pm=$p; break; fi
done
missing=()
have cc || missing+=(compiler)
have pkg-config || missing+=(pkg-config)
if have pkg-config; then
    pkg-config --exists alsa || missing+=(alsa)
    pkg-config --exists libudev || missing+=(udev)
fi
have ffmpeg || missing+=(ffmpeg)
if ((${#missing[@]})); then
    # Package names per distribution for each missing piece.
    declare -A pkgs
    case $pm in
        pacman)  pkgs=([compiler]=base-devel [pkg-config]=pkgconf [alsa]=alsa-lib [udev]=systemd-libs [ffmpeg]=ffmpeg); install="sudo pacman -S --needed" ;;
        apt-get) pkgs=([compiler]=build-essential [pkg-config]=pkg-config [alsa]=libasound2-dev [udev]=libudev-dev [ffmpeg]=ffmpeg); install="sudo apt-get install" ;;
        dnf)     pkgs=([compiler]=gcc [pkg-config]=pkgconf-pkg-config [alsa]=alsa-lib-devel [udev]=systemd-devel [ffmpeg]=ffmpeg-free); install="sudo dnf install" ;;
        zypper)  pkgs=([compiler]=gcc [pkg-config]=pkg-config [alsa]=alsa-devel [udev]=libudev-devel [ffmpeg]=ffmpeg); install="sudo zypper install" ;;
        *) fail "missing: ${missing[*]}. Install them with your package manager (ffmpeg is optional, for the movies) and run this again." ;;
    esac
    names=()
    for m in "${missing[@]}"; do names+=("${pkgs[$m]}"); done
    echo "missing: ${missing[*]} (ffmpeg is only needed for the movies)"
    if ask "Install them now with: $install ${names[*]} ?"; then
        $install "${names[@]}"
    elif [[ " ${missing[*]} " != " ffmpeg " ]]; then
        fail "can't build without them."
    fi
fi
if ! have cargo; then
    [[ -x $HOME/.cargo/bin/cargo ]] && export PATH="$HOME/.cargo/bin:$PATH"
fi
if ! have cargo; then
    echo "Rust is not installed."
    if ask "Install it with rustup (https://rustup.rs, into ~/.cargo)?"; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
        export PATH="$HOME/.cargo/bin:$PATH"
    else
        fail "Rust is needed to build the port."
    fi
fi
echo "Rust: $(cargo --version)"

# ---- 3. The game's program ----
step "Copying the game's program off the disc"
if [[ ! -f game/SLUS_203.67 ]]; then
    cargo run --release -q -p ee-runtime --bin disc-extract
fi
sum=$(sha256sum game/SLUS_203.67 | cut -d' ' -f1)
[[ $sum == "$SLUS_SHA256" ]] || fail "game/SLUS_203.67 is not the NTSC-U release this port needs (SLUS-20367). Delete it and use a dump of that disc."
echo "game/SLUS_203.67 OK"

# ---- 4. Generate the game code and build ----
step "Generating the game code and building (a few minutes the first time)"
cargo run --release -q -p ee-recomp -- game/SLUS_203.67 freekstyle/src/gen freekstyle/hle.txt freekstyle/seeds.txt freekstyle/vu_dumps
cargo build --release -p freekstyle -p freekstyle-launcher -p flm

# ---- 5. Pictures ----
step "Converting the title and menu pictures"
if compgen -G "game/images/*.png" >/dev/null; then
    echo "already done"
else
    cargo run --release -q -p ee-runtime --bin lsc2png
fi

# ---- 6. Movies ----
step "Converting the movies"
if ! have ffmpeg; then
    echo "skipped: no ffmpeg (the game skips its movies)"
elif [[ -f game/movies/hd/intro.mkv || -f game/movies/intro.mkv ]]; then
    echo "already done"
else
    for m in $MOVIES; do
        [[ -f game/movies/flm/${m#*/}.FLM ]] || cargo run --release -q -p ee-runtime --bin disc-extract -- game/Freekstyle.bin "\\SXDATA\\${m%/*}\\${m#*/}.FLM" game/movies/flm
    done
    # The CRT look needs ffmpeg's libplacebo filter and a Vulkan GPU.
    # (grep without -q: it would close the pipe early and fail the pipeline.)
    if ffmpeg -hide_banner -filters 2>/dev/null | grep libplacebo >/dev/null &&
        ./target/release/flm2mkv --crt game/movies/flm game/movies/hd; then
        :
    else
        echo "CRT look unavailable; converting the movies without it"
        rm -rf game/movies/hd
        ./target/release/flm2mkv game/movies/flm game/movies
    fi
fi

step "Done"
cat <<'EOF'
Start the game with:   ./target/release/freekstyle-launcher

Races run on the VU interpreter until their programs are recorded. For
60 fps races, play a race or two with
    FREEK_VU_DUMP=freekstyle/vu_dumps ./target/release/freekstyle
then run ./setup.sh again.
EOF
