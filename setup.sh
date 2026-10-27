#!/usr/bin/env bash
# Set up Freekstyle from your own disc dump: put the dump (.bin or .iso) in
# game/ and run ./setup.sh. It installs what is missing (asking first),
# copies what the port needs off the disc, generates the game code and
# builds it. Steps already done are skipped, so it is safe to run again
# (e.g. after recording VU programs, see README). At the end it offers to
# install the game to the system (/opt/freekstyle, with the launcher in the
# app menu's Games section); ./setup.sh --uninstall removes that again.
# It speaks English, Brazilian Portuguese, Spanish or French, following the
# system language like the launcher.
set -euo pipefail
cd "$(dirname "$0")"

SLUS_SHA256=debcb4b0731741a389a0c890296609eb99d111952e97a250ea4c3cfc04a6d80a
MOVIES="OPENING/BIGLOGO OPENING/EAGAMES OPENING/EAS_BIG OPENING/INTRO ABOUTEA/PROD1"
PREFIX=/opt/freekstyle
MENU=(/usr/share/applications/freekstyle.desktop /usr/share/applications/freekstyle-launcher.desktop)

# ---- Messages ----
# The language comes from the first of LANGUAGE, LC_ALL, LC_MESSAGES, LANG
# that is set, as in the launcher (launcher/src/i18n.rs).
locale=""
for v in LANGUAGE LC_ALL LC_MESSAGES LANG; do
    if [[ -n ${!v:-} ]]; then locale=${!v}; break; fi
done
case ${locale:0:2} in
    pt) L=1 ;;
    es) L=2 ;;
    fr) L=3 ;;
    *) L=0 ;;
esac
declare -A MSG
# msg <key> <English> <Portuguese (Brazil)> <Spanish> <French>; texts are
# printf formats (%s for arguments).
msg() { local all=("${@:2}"); MSG[$1]=${all[$L]}; }
# t <key> [args...]: the message in the system language.
t() { local f=${MSG[$1]}; shift; printf "$f" "$@"; }

msg yes_no "[Y/n]" "[S/n]" "[S/n]" "[O/n]"
msg error "error:" "erro:" "error:" "erreur :"
msg look_dump "Looking for your disc dump in game/" "Procurando a imagem do disco em game/" "Buscando la imagen del disco en game/" "Recherche de l'image du disque dans game/"
msg no_dump "no disc image in game/. Dump your Freekstyle disc (NTSC-U, SLUS-20367) as a .bin or .iso into game/, then run this again." \
    "nenhuma imagem de disco em game/. Copie seu disco do Freekstyle (NTSC-U, SLUS-20367) como .bin ou .iso para game/ e rode de novo." \
    "no hay imagen de disco en game/. Copia tu disco de Freekstyle (NTSC-U, SLUS-20367) como .bin o .iso en game/ y vuelve a ejecutarlo." \
    "aucune image disque dans game/. Copiez votre disque Freekstyle (NTSC-U, SLUS-20367) en .bin ou .iso dans game/, puis relancez."
msg using_dump "using %s (linked as game/Freekstyle.bin)" "usando %s (ligado como game/Freekstyle.bin)" "usando %s (enlazado como game/Freekstyle.bin)" "utilisation de %s (lié en game/Freekstyle.bin)"
msg many_dumps "several disc images in game/ (%s); rename yours to Freekstyle.bin." "várias imagens de disco em game/ (%s); renomeie a sua para Freekstyle.bin." \
    "varias imágenes de disco en game/ (%s); renombra la tuya a Freekstyle.bin." "plusieurs images disque dans game/ (%s) ; renommez la vôtre en Freekstyle.bin."
msg found_dump "found game/Freekstyle.bin" "encontrado game/Freekstyle.bin" "encontrado game/Freekstyle.bin" "trouvé game/Freekstyle.bin"
msg check_tools "Checking tools and libraries" "Verificando ferramentas e bibliotecas" "Comprobando herramientas y bibliotecas" "Vérification des outils et bibliothèques"
msg missing_nopm "missing: %s. Install them with your package manager (ffmpeg is optional, for the movies) and run this again." \
    "faltando: %s. Instale com o gerenciador de pacotes (ffmpeg é opcional, para os vídeos) e rode de novo." \
    "faltan: %s. Instálalos con tu gestor de paquetes (ffmpeg es opcional, para los vídeos) y vuelve a ejecutarlo." \
    "manquant : %s. Installez-les avec votre gestionnaire de paquets (ffmpeg est facultatif, pour les vidéos) et relancez."
msg missing "missing: %s (vulkan is the graphics driver the game draws with; ffmpeg is only needed for the movies)" \
    "faltando: %s (vulkan é o driver gráfico que o jogo usa para desenhar; ffmpeg só é necessário para os vídeos)" \
    "faltan: %s (vulkan es el controlador gráfico con el que dibuja el juego; ffmpeg solo hace falta para los vídeos)" \
    "manquant : %s (vulkan est le pilote graphique avec lequel le jeu dessine ; ffmpeg ne sert qu'aux vidéos)"
msg install_now "Install them now with: %s ?" "Instalar agora com: %s ?" "¿Instalarlos ahora con: %s ?" "Les installer maintenant avec : %s ?"
msg cant_build "can't build without them." "não dá para compilar sem eles." "no se puede compilar sin ellos." "impossible de compiler sans eux."
msg rust_old "Rust %s is too old: the port needs %s or newer." "Rust %s é antiga demais: o port precisa da %s ou mais nova." \
    "Rust %s es demasiado antiguo: el port necesita %s o más reciente." "Rust %s est trop ancien : le portage demande %s ou plus récent."
msg rust_none "Rust is not installed." "Rust não está instalada." "Rust no está instalado." "Rust n'est pas installé."
msg rustup_update "Update it with: rustup default stable && rustup update stable ?" "Atualizar com: rustup default stable && rustup update stable ?" \
    "¿Actualizarlo con: rustup default stable && rustup update stable ?" "Le mettre à jour avec : rustup default stable && rustup update stable ?"
msg rustup_install "Install current Rust with rustup (https://rustup.rs, into ~/.cargo; a Rust from your distribution stays installed but isn't used here)?" \
    "Instalar a Rust atual com rustup (https://rustup.rs, em ~/.cargo; a Rust da sua distribuição continua instalada, mas não é usada aqui)?" \
    "¿Instalar Rust actual con rustup (https://rustup.rs, en ~/.cargo; el Rust de tu distribución sigue instalado pero no se usa aquí)?" \
    "Installer Rust à jour avec rustup (https://rustup.rs, dans ~/.cargo ; le Rust de votre distribution reste installé mais n'est pas utilisé ici) ?"
msg rust_needed "Rust %s or newer is needed." "É preciso Rust %s ou mais nova." "Hace falta Rust %s o más reciente." "Rust %s ou plus récent est nécessaire."
msg rust_still "still no Rust %s or newer (found: %s)." "ainda sem Rust %s ou mais nova (encontrada: %s)." \
    "todavía no hay Rust %s o más reciente (encontrado: %s)." "toujours pas de Rust %s ou plus récent (trouvé : %s)."
msg copy_prog "Copying the game's program off the disc" "Copiando o programa do jogo do disco" "Copiando el programa del juego del disco" "Copie du programme du jeu depuis le disque"
msg bad_slus "game/SLUS_203.67 is not the NTSC-U release this port needs (SLUS-20367). Delete it and use a dump of that disc." \
    "game/SLUS_203.67 não é a versão NTSC-U de que o port precisa (SLUS-20367). Apague-o e use uma cópia desse disco." \
    "game/SLUS_203.67 no es la versión NTSC-U que necesita el port (SLUS-20367). Bórralo y usa una copia de ese disco." \
    "game/SLUS_203.67 n'est pas la version NTSC-U dont le portage a besoin (SLUS-20367). Supprimez-le et utilisez une copie de ce disque."
msg building "Generating the game code and building (a few minutes the first time)" "Gerando o código do jogo e compilando (alguns minutos na primeira vez)" \
    "Generando el código del juego y compilando (unos minutos la primera vez)" "Génération du code du jeu et compilation (quelques minutes la première fois)"
msg pictures "Converting the title and menu pictures" "Convertendo as imagens de título e menu" "Convirtiendo las imágenes de título y menú" "Conversion des images de titre et de menu"
msg already "already done" "já feito" "ya hecho" "déjà fait"
msg movies "Converting the movies" "Convertendo os vídeos" "Convirtiendo los vídeos" "Conversion des vidéos"
msg no_ffmpeg "skipped: no ffmpeg (the game skips its movies)" "pulado: sem ffmpeg (o jogo pula os vídeos)" \
    "omitido: no hay ffmpeg (el juego omite sus vídeos)" "ignoré : pas de ffmpeg (le jeu saute ses vidéos)"
msg no_mpeg2 "skipped: this ffmpeg can't decode the movies (MPEG-2), so the game skips them. Fedora's ffmpeg-free lacks it: install the full ffmpeg from RPM Fusion (https://rpmfusion.org/Howto/Multimedia), then run this again." \
    "pulado: este ffmpeg não decodifica os vídeos (MPEG-2), então o jogo os pula. O ffmpeg-free do Fedora não tem isso: instale o ffmpeg completo do RPM Fusion (https://rpmfusion.org/Howto/Multimedia) e rode de novo." \
    "omitido: este ffmpeg no decodifica los vídeos (MPEG-2), así que el juego los omite. El ffmpeg-free de Fedora no lo trae: instala el ffmpeg completo de RPM Fusion (https://rpmfusion.org/Howto/Multimedia) y vuelve a ejecutarlo." \
    "ignoré : ce ffmpeg ne décode pas les vidéos (MPEG-2), le jeu les saute donc. Le ffmpeg-free de Fedora ne le fait pas : installez le ffmpeg complet de RPM Fusion (https://rpmfusion.org/Howto/Multimedia), puis relancez."
msg no_crt "CRT look unavailable (needs ffmpeg with libplacebo and a GPU); converting the movies without it" \
    "visual CRT indisponível (precisa de ffmpeg com libplacebo e uma GPU); convertendo os vídeos sem ele" \
    "aspecto CRT no disponible (necesita ffmpeg con libplacebo y una GPU); convirtiendo los vídeos sin él" \
    "rendu CRT indisponible (il faut ffmpeg avec libplacebo et un GPU) ; conversion des vidéos sans lui"
msg movies_failed "some movies could not be converted (see above); the game skips those." "alguns vídeos não puderam ser convertidos (veja acima); o jogo os pula." \
    "algunos vídeos no se pudieron convertir (ver arriba); el juego los omite." "certaines vidéos n'ont pas pu être converties (voir plus haut) ; le jeu les saute."
msg installing "Installing" "Instalando" "Instalando" "Installation"
msg opt_system "  1) Install to the system: %s, with Freekstyle in the app menu (Games); needs sudo" \
    "  1) Instalar no sistema: %s, com o Freekstyle no menu de aplicativos (Jogos); precisa de sudo" \
    "  1) Instalar en el sistema: %s, con Freekstyle en el menú de aplicaciones (Juegos); necesita sudo" \
    "  1) Installer dans le système : %s, avec Freekstyle dans le menu des applications (Jeux) ; demande sudo"
msg opt_here "  2) Keep it in this folder" "  2) Deixar nesta pasta" "  2) Dejarlo en esta carpeta" "  2) Le garder dans ce dossier"
msg choose "Choose [1]: " "Escolha [1]: " "Elige [1]: " "Choix [1] : "
msg copying_disc "copying the disc image..." "copiando a imagem do disco..." "copiando la imagen del disco..." "copie de l'image du disque..."
msg done "Done" "Pronto" "Listo" "Terminé"
msg done_system "Freekstyle is in your app menu under Games (also: freekstyle-launcher in a
terminal). This folder isn't needed to play any more; ./setup.sh --uninstall
removes the installed copy." \
    "O Freekstyle está no menu de aplicativos, em Jogos (ou: freekstyle-launcher
no terminal). Esta pasta não é mais necessária para jogar; ./setup.sh --uninstall
remove a cópia instalada." \
    "Freekstyle está en el menú de aplicaciones, en Juegos (o: freekstyle-launcher
en una terminal). Esta carpeta ya no hace falta para jugar; ./setup.sh --uninstall
quita la copia instalada." \
    "Freekstyle est dans le menu des applications, dans Jeux (ou : freekstyle-launcher
dans un terminal). Ce dossier n'est plus nécessaire pour jouer ; ./setup.sh --uninstall
supprime la copie installée."
msg done_here "Start the game with:   ./target/release/freekstyle-launcher" "Inicie o jogo com:   ./target/release/freekstyle-launcher" \
    "Inicia el juego con:   ./target/release/freekstyle-launcher" "Lancez le jeu avec :   ./target/release/freekstyle-launcher"
msg vu_note "Races run on the VU interpreter until their programs are recorded. For
60 fps races, play a race or two from this folder with
    FREEK_VU_DUMP=freekstyle/vu_dumps ./target/release/freekstyle
then run ./setup.sh again." \
    "As corridas usam o interpretador de VU até seus programas serem gravados. Para
corridas a 60 fps, jogue uma ou duas corridas a partir desta pasta com
    FREEK_VU_DUMP=freekstyle/vu_dumps ./target/release/freekstyle
e rode ./setup.sh de novo." \
    "Las carreras usan el intérprete de VU hasta que se graban sus programas. Para
carreras a 60 fps, juega una o dos carreras desde esta carpeta con
    FREEK_VU_DUMP=freekstyle/vu_dumps ./target/release/freekstyle
y vuelve a ejecutar ./setup.sh." \
    "Les courses passent par l'interpréteur VU tant que leurs programmes ne sont pas
enregistrés. Pour des courses à 60 fps, jouez une course ou deux depuis ce dossier avec
    FREEK_VU_DUMP=freekstyle/vu_dumps ./target/release/freekstyle
puis relancez ./setup.sh."
msg not_installed "Freekstyle is not installed in %s." "O Freekstyle não está instalado em %s." "Freekstyle no está instalado en %s." "Freekstyle n'est pas installé dans %s."
msg uninstall_info "This removes %s and its app menu entry. Your saves (~/.local/share/freekstyle) and settings (~/.config/freekstyle) stay." \
    "Isto remove %s e o item do menu. Seus saves (~/.local/share/freekstyle) e configurações (~/.config/freekstyle) ficam." \
    "Esto quita %s y su entrada del menú. Tus partidas guardadas (~/.local/share/freekstyle) y ajustes (~/.config/freekstyle) se conservan." \
    "Cela supprime %s et son entrée du menu. Vos sauvegardes (~/.local/share/freekstyle) et réglages (~/.config/freekstyle) restent."
msg uninstall_q "Uninstall?" "Desinstalar?" "¿Desinstalar?" "Désinstaller ?"
msg uninstalled "Uninstalled." "Desinstalado." "Desinstalado." "Désinstallé."

step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
fail() { printf '\033[31m%s\033[0m %s\n' "$(t error)" "$*" >&2; exit 1; }
# ask <question>: yes (the default) in English or the system language.
ask() {
    local reply
    read -r -p "$1 $(t yes_no) " reply || reply=n
    [[ -z $reply || $reply == [YySsOo]* ]]
}
have() { command -v "$1" >/dev/null 2>&1; }

# Let the desktops see menu and icon changes now rather than at next login.
refresh_menus() {
    have update-desktop-database && sudo update-desktop-database -q /usr/share/applications || true
    have gtk-update-icon-cache && sudo gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || true
    have kbuildsycoca6 && kbuildsycoca6 >/dev/null 2>&1 || true
}

if [[ ${1:-} == --uninstall ]]; then
    [[ -e $PREFIX ]] || fail "$(t not_installed "$PREFIX")"
    t uninstall_info "$PREFIX"; echo
    ask "$(t uninstall_q)" || exit 0
    sudo rm -rf "$PREFIX" /usr/local/bin/freekstyle-launcher "${MENU[@]}"
    sudo find /usr/share/icons/hicolor -path '*/apps/freekstyle*.png' -delete
    refresh_menus
    t uninstalled; echo
    exit 0
fi

# ---- 1. The disc dump ----
step "$(t look_dump)"
mkdir -p game
if [[ ! -e game/Freekstyle.bin ]]; then
    shopt -s nullglob nocaseglob
    dumps=(game/*.bin game/*.iso)
    shopt -u nullglob nocaseglob
    case ${#dumps[@]} in
        0) fail "$(t no_dump)" ;;
        1) ln -s "$(basename "${dumps[0]}")" game/Freekstyle.bin
           t using_dump "${dumps[0]}"; echo ;;
        *) fail "$(t many_dumps "${dumps[*]}")" ;;
    esac
else
    t found_dump; echo
fi

# ---- 2. Tools and libraries ----
step "$(t check_tools)"
# rustup's Rust (~/.cargo/bin) goes before a distribution's, which is often
# older than the dependencies need.
[[ -d $HOME/.cargo/bin ]] && export PATH="$HOME/.cargo/bin:$PATH"
MIN_RUST=$(sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml)
rust_ok() {
    have cargo && have rustc || return 1
    local v
    v=$(rustc --version | cut -d' ' -f2)
    [[ $(printf '%s\n%s\n' "$MIN_RUST" "$v" | sort -V | head -1) == "$MIN_RUST" ]]
}
# The game draws with Vulkan: it needs the loader and a driver (an ICD).
vulkan_ok() {
    compgen -G "/usr/share/vulkan/icd.d/*.json" >/dev/null || compgen -G "/etc/vulkan/icd.d/*.json" >/dev/null || return 1
    local lib
    for lib in /usr/lib/libvulkan.so.1 /usr/lib64/libvulkan.so.1 /usr/lib/*-linux-gnu/libvulkan.so.1; do
        [[ -e $lib ]] && return 0
    done
    return 1
}
# GPU makers present (amd, intel, nvidia), for distributions that package
# each Vulkan driver separately.
gpus() {
    local d
    for d in /sys/bus/pci/devices/*; do
        [[ $(cat "$d/class" 2>/dev/null) == 0x03* ]] || continue
        case $(cat "$d/vendor" 2>/dev/null) in
            0x1002) echo amd ;;
            0x8086) echo intel ;;
            0x10de) echo nvidia ;;
        esac
    done | sort -u
}
# vulkan_drivers <amd> <intel> <nouveau> <nvidia> <software>: the Vulkan
# driver packages for this machine's GPUs (the software one if none is
# recognised, as in most virtual machines).
vulkan_drivers() {
    local out=() g
    for g in $(gpus); do
        case $g in
            amd) out+=("$1") ;;
            intel) out+=("$2") ;;
            nvidia) if [[ -d /sys/module/nvidia ]]; then out+=("$4"); else out+=("$3"); fi ;;
        esac
    done
    ((${#out[@]})) || out=("$5")
    echo "${out[*]}"
}
pm=""
for p in pacman apt-get dnf zypper; do
    if have "$p"; then pm=$p; break; fi
done
missing=()
have cc || missing+=(compiler)
have pkg-config || missing+=(pkg-config)
# The renderer's shaders are compiled at build time.
have glslc || missing+=(glslc)
if have pkg-config; then
    pkg-config --exists alsa || missing+=(alsa)
    pkg-config --exists libudev || missing+=(udev)
fi
vulkan_ok || missing+=(vulkan)
have ffmpeg || missing+=(ffmpeg)
# rustup is fetched with curl.
if ! rust_ok && ! have rustup && ! have curl; then
    missing+=(curl)
fi
if ((${#missing[@]})); then
    # Package names per distribution for each missing piece.
    declare -A pkgs
    case $pm in
        pacman)  pkgs=([compiler]=base-devel [pkg-config]=pkgconf [alsa]=alsa-lib [udev]=systemd-libs [ffmpeg]=ffmpeg [curl]=curl [glslc]=shaderc
                       [vulkan]="vulkan-icd-loader $(vulkan_drivers vulkan-radeon vulkan-intel vulkan-nouveau nvidia-utils vulkan-swrast)")
                 install="sudo pacman -S --needed" ;;
        apt-get) pkgs=([compiler]=build-essential [pkg-config]=pkg-config [alsa]=libasound2-dev [udev]=libudev-dev [ffmpeg]=ffmpeg [curl]=curl [glslc]=glslc
                       [vulkan]="libvulkan1 mesa-vulkan-drivers")
                 install="sudo apt-get install" ;;
        dnf)     pkgs=([compiler]=gcc [pkg-config]=pkgconf-pkg-config [alsa]=alsa-lib-devel [udev]=systemd-devel [ffmpeg]=ffmpeg-free [curl]=curl [glslc]=glslc
                       [vulkan]="vulkan-loader mesa-vulkan-drivers")
                 install="sudo dnf install" ;;
        zypper)  pkgs=([compiler]=gcc [pkg-config]=pkg-config [alsa]=alsa-devel [udev]=libudev-devel [ffmpeg]=ffmpeg [curl]=curl [glslc]=shaderc
                       [vulkan]="libvulkan1 $(vulkan_drivers libvulkan_radeon libvulkan_intel libvulkan_lvp libvulkan_lvp libvulkan_lvp)")
                 install="sudo zypper install" ;;
        *) fail "$(t missing_nopm "${missing[*]}")" ;;
    esac
    names=()
    for m in "${missing[@]}"; do
        read -r -a p <<<"${pkgs[$m]}"
        names+=("${p[@]}")
    done
    t missing "${missing[*]}"; echo
    if ask "$(t install_now "$install ${names[*]}")"; then
        $install "${names[@]}"
    elif [[ " ${missing[*]} " != " ffmpeg " ]]; then
        fail "$(t cant_build)"
    fi
fi
if ! rust_ok; then
    if have rustc; then
        t rust_old "$(rustc --version | cut -d' ' -f2)" "$MIN_RUST"; echo
    else
        t rust_none; echo
    fi
    if have rustup; then
        ask "$(t rustup_update)" || fail "$(t rust_needed "$MIN_RUST")"
        rustup default stable
        rustup update stable
    elif ask "$(t rustup_install)"; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
        export PATH="$HOME/.cargo/bin:$PATH"
    else
        fail "$(t rust_needed "$MIN_RUST")"
    fi
    rust_ok || fail "$(t rust_still "$MIN_RUST" "$(rustc --version 2>/dev/null || echo -)")"
fi
echo "Rust: $(rustc --version)"

# ---- 3. The game's program ----
step "$(t copy_prog)"
if [[ ! -f game/SLUS_203.67 ]]; then
    cargo run --release -q -p ee-runtime --bin disc-extract
fi
sum=$(sha256sum game/SLUS_203.67 | cut -d' ' -f1)
[[ $sum == "$SLUS_SHA256" ]] || fail "$(t bad_slus)"
echo "game/SLUS_203.67 OK"

# ---- 4. Generate the game code and build ----
step "$(t building)"
cargo run --release -q -p ee-recomp -- game/SLUS_203.67 freekstyle/src/gen freekstyle/hle.txt freekstyle/seeds.txt freekstyle/vu_dumps
cargo build --release -p freekstyle -p freekstyle-launcher -p flm

# ---- 5. Pictures ----
step "$(t pictures)"
if compgen -G "game/images/*.png" >/dev/null; then
    t already; echo
else
    cargo run --release -q -p ee-runtime --bin lsc2png
fi

# ---- 6. Movies ----
step "$(t movies)"
if ! have ffmpeg; then
    t no_ffmpeg; echo
elif [[ -f game/movies/hd/intro.mkv || -f game/movies/intro.mkv ]]; then
    t already; echo
# The movies are MPEG-2, which some ffmpeg builds (Fedora's ffmpeg-free)
# leave out. (grep without -q: it would close the pipe early and fail the
# pipeline.)
elif ! ffmpeg -hide_banner -decoders 2>/dev/null | grep -E '^ V.{5} mpeg2video ' >/dev/null; then
    t no_mpeg2; echo
else
    for m in $MOVIES; do
        [[ -f game/movies/flm/${m#*/}.FLM ]] || cargo run --release -q -p ee-runtime --bin disc-extract -- game/Freekstyle.bin "\\SXDATA\\${m%/*}\\${m#*/}.FLM" game/movies/flm
    done
    # The CRT look needs ffmpeg's libplacebo filter and a Vulkan GPU (not a
    # software one, as in most virtual machines): try it on a tiny clip first.
    if ffmpeg -hide_banner -loglevel quiet -f lavfi -i color=size=64x64:duration=0.1 -vf libplacebo -f null - </dev/null &&
        ./target/release/flm2mkv --crt game/movies/flm game/movies/hd; then
        :
    else
        t no_crt; echo
        rm -rf game/movies/hd
        ./target/release/flm2mkv game/movies/flm game/movies || { t movies_failed; echo; }
    fi
fi

# ---- 7. Install ----
step "$(t installing)"
t opt_system "$PREFIX"; echo
t opt_here; echo
read -r -p "$(t choose)" where || where=2
if [[ ${where:-1} == 1 ]]; then
    sudo rm -rf "$PREFIX/game"
    sudo mkdir -p "$PREFIX/game/movies"
    sudo install -m 755 target/release/freekstyle target/release/freekstyle-launcher "$PREFIX/"
    sudo install -m 644 game/SLUS_203.67 "$PREFIX/game/"
    t copying_disc; echo
    sudo cp -L --reflink=auto game/Freekstyle.bin "$PREFIX/game/Freekstyle.bin"
    if [[ -d game/images ]]; then sudo cp -r game/images "$PREFIX/game/"; fi
    if [[ -d game/movies/hd ]]; then sudo cp -r game/movies/hd "$PREFIX/game/movies/"; fi
    if compgen -G "game/movies/*.mkv" >/dev/null; then sudo cp game/movies/*.mkv "$PREFIX/game/movies/"; fi
    sudo chmod -R a+rX "$PREFIX"
    sudo ln -sf "$PREFIX/freekstyle-launcher" /usr/local/bin/freekstyle-launcher
    # Run from the install, so the entries point there (not at this folder).
    (cd "$PREFIX" && sudo ./freekstyle-launcher --install-menu /usr/share)
    # Your entries from running it in this folder would hide the system one.
    user_data=${XDG_DATA_HOME:-$HOME/.local/share}
    rm -f "$user_data"/applications/freekstyle{,-launcher}.desktop
    find "$user_data/icons/hicolor" -path '*/apps/freekstyle*.png' -delete 2>/dev/null || true
    # Settings made here carry over (the install directory is read-only).
    config=${XDG_CONFIG_HOME:-$HOME/.config}/freekstyle
    if [[ -f freekstyle.toml && ! -f $config/freekstyle.toml ]]; then
        mkdir -p "$config" && cp freekstyle.toml "$config/"
    fi
    refresh_menus
    step "$(t done)"
    t done_system; printf '\n\n'
else
    step "$(t done)"
    t done_here; printf '\n\n'
fi
t vu_note; echo
