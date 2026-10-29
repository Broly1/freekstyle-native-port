#!/usr/bin/env python3
"""Set up Freekstyle from your own disc dump.

Put the dump (.bin or .iso) in game/ and run ./install.py. It installs what
is missing (asking first), copies what the port needs off the disc,
generates the game code and builds it, converts the pictures and movies,
and offers to install the game to the system (/opt/freekstyle, with the
launcher in the app menu's Games section). Steps already done are skipped,
so it is safe to run again after a `git pull`. What it adds (packages,
Rust, files, the system install) is recorded in
~/.local/state/freekstyle/install.json, so that ./uninstall.py can remove
exactly that and nothing else.

It speaks English, Brazilian Portuguese, Spanish or French, following the
system language like the launcher. Everything it runs is logged to
install.log. Only the Python standard library is used.
"""

import argparse
import glob
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent
GAME = ROOT / "game"
LOG = ROOT / "install.log"
SLUS_SHA256 = "debcb4b0731741a389a0c890296609eb99d111952e97a250ea4c3cfc04a6d80a"
MOVIES = ["OPENING/BIGLOGO", "OPENING/EAGAMES", "OPENING/EAS_BIG", "OPENING/INTRO", "ABOUTEA/PROD1"]
PREFIX = Path("/opt/freekstyle")
MENU = [Path("/usr/share/applications/freekstyle.desktop"), Path("/usr/share/applications/freekstyle-launcher.desktop")]
# Free space wanted for the build (it ends up around 7 GB) and the copies.
SPACE_GB = 10
# What this script added (packages, Rust, RPM Fusion, files, the system
# install), for uninstall.py: kept outside the source folder so it
# survives that folder being deleted.
STATE = Path(os.environ.get("XDG_STATE_HOME") or Path.home() / ".local" / "state") / "freekstyle" / "install.json"

# ---- Messages ----

def _language():
    """The first of LANGUAGE, LC_ALL, LC_MESSAGES, LANG that is set, as in
    the launcher (launcher/src/i18n.rs): 0 English, 1 Portuguese (Brazil),
    2 Spanish, 3 French."""
    for v in ("LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"):
        value = os.environ.get(v)
        if value:
            return {"pt": 1, "es": 2, "fr": 3}.get(value[:2], 0)
    return 0

LANG = _language()

# key: (English, Portuguese (Brazil), Spanish, French); %s for arguments.
MESSAGES = {
    "yes_no": ("[Y/n]", "[S/n]", "[S/n]", "[O/n]"),
    "yes_no_no": ("[y/N]", "[s/N]", "[s/N]", "[o/N]"),
    "error": ("error:", "erro:", "error:", "erreur :"),
    "warning": ("warning:", "aviso:", "aviso:", "attention :"),
    "root": ("run this as your own user, not as root: it asks for sudo when it needs it.",
             "rode como seu usuário, não como root: ele pede sudo quando precisa.",
             "ejecútalo como tu usuario, no como root: pide sudo cuando lo necesita.",
             "lancez-le avec votre utilisateur, pas en root : il demande sudo quand il en a besoin."),
    "look_dump": ("Your disc dump", "Imagem do disco", "Imagen del disco", "Image du disque"),
    "no_dump": ("no disc image in game/. Dump your Freekstyle disc (NTSC-U, SLUS-20367) as a .bin or .iso into game/, then run this again.",
                "nenhuma imagem de disco em game/. Copie seu disco do Freekstyle (NTSC-U, SLUS-20367) como .bin ou .iso para game/ e rode de novo.",
                "no hay imagen de disco en game/. Copia tu disco de Freekstyle (NTSC-U, SLUS-20367) como .bin o .iso en game/ y vuelve a ejecutarlo.",
                "aucune image disque dans game/. Copiez votre disque Freekstyle (NTSC-U, SLUS-20367) en .bin ou .iso dans game/, puis relancez."),
    "using_dump": ("using %s (linked as game/Freekstyle.bin)", "usando %s (ligado como game/Freekstyle.bin)",
                   "usando %s (enlazado como game/Freekstyle.bin)", "utilisation de %s (lié en game/Freekstyle.bin)"),
    "many_dumps": ("several disc images in game/ (%s); rename yours to Freekstyle.bin.",
                   "várias imagens de disco em game/ (%s); renomeie a sua para Freekstyle.bin.",
                   "varias imágenes de disco en game/ (%s); renombra la tuya a Freekstyle.bin.",
                   "plusieurs images disque dans game/ (%s) ; renommez la vôtre en Freekstyle.bin."),
    "found_dump": ("found game/Freekstyle.bin", "encontrado game/Freekstyle.bin", "encontrado game/Freekstyle.bin", "trouvé game/Freekstyle.bin"),
    "space": ("only %s GB free here; the build needs about %s GB.", "só %s GB livres aqui; a compilação precisa de uns %s GB.",
              "solo %s GB libres aquí; la compilación necesita unos %s GB.", "seulement %s Go libres ici ; la compilation demande environ %s Go."),
    "continue": ("Continue anyway?", "Continuar mesmo assim?", "¿Continuar de todos modos?", "Continuer quand même ?"),
    "check_tools": ("Tools and libraries", "Ferramentas e bibliotecas", "Herramientas y bibliotecas", "Outils et bibliothèques"),
    "req_compiler": ("C compiler and linker", "compilador C e linker", "compilador C y enlazador", "compilateur C et éditeur de liens"),
    "req_pkgconfig": ("pkg-config", "pkg-config", "pkg-config", "pkg-config"),
    "req_glslc": ("glslc (shader compiler)", "glslc (compilador de shaders)", "glslc (compilador de shaders)", "glslc (compilateur de shaders)"),
    "req_alsa": ("ALSA headers (sound)", "cabeçalhos ALSA (som)", "cabeceras ALSA (sonido)", "en-têtes ALSA (son)"),
    "req_udev": ("libudev headers (controllers)", "cabeçalhos libudev (controles)", "cabeceras libudev (mandos)", "en-têtes libudev (manettes)"),
    "req_vulkan": ("Vulkan loader and driver (graphics)", "Vulkan: carregador e driver (gráficos)", "Vulkan: cargador y controlador (gráficos)", "Vulkan : chargeur et pilote (graphismes)"),
    "req_ffmpeg": ("ffmpeg (movies, optional)", "ffmpeg (vídeos, opcional)", "ffmpeg (vídeos, opcional)", "ffmpeg (vidéos, facultatif)"),
    "req_curl": ("curl (to fetch Rust)", "curl (para baixar a Rust)", "curl (para descargar Rust)", "curl (pour télécharger Rust)"),
    "missing_nopm": ("missing: %s. Install them with your package manager and run this again.",
                     "faltando: %s. Instale com o gerenciador de pacotes e rode de novo.",
                     "faltan: %s. Instálalos con tu gestor de paquetes y vuelve a ejecutarlo.",
                     "manquant : %s. Installez-les avec votre gestionnaire de paquets et relancez."),
    "install_now": ("Install the missing packages now?\n  %s", "Instalar os pacotes que faltam agora?\n  %s",
                    "¿Instalar ahora los paquetes que faltan?\n  %s", "Installer maintenant les paquets manquants ?\n  %s"),
    "cant_build": ("can't build without them.", "não dá para compilar sem eles.", "no se puede compilar sin ellos.", "impossible de compiler sans eux."),
    "still_missing": ("still missing after installing: %s", "ainda faltando depois de instalar: %s",
                      "todavía faltan después de instalar: %s", "toujours manquant après l'installation : %s"),
    "rpmfusion": ("Fedora's own ffmpeg (ffmpeg-free) can't decode the game's movies (MPEG-2). The full ffmpeg comes from the RPM Fusion repository (https://rpmfusion.org). Enable RPM Fusion (free) and install it?",
                  "O ffmpeg do Fedora (ffmpeg-free) não decodifica os vídeos do jogo (MPEG-2). O ffmpeg completo vem do repositório RPM Fusion (https://rpmfusion.org). Ativar o RPM Fusion (free) e instalá-lo?",
                  "El ffmpeg de Fedora (ffmpeg-free) no decodifica los vídeos del juego (MPEG-2). El ffmpeg completo viene del repositorio RPM Fusion (https://rpmfusion.org). ¿Activar RPM Fusion (free) e instalarlo?",
                  "Le ffmpeg de Fedora (ffmpeg-free) ne décode pas les vidéos du jeu (MPEG-2). Le ffmpeg complet vient du dépôt RPM Fusion (https://rpmfusion.org). Activer RPM Fusion (free) et l'installer ?"),
    "rust": ("Rust", "Rust", "Rust", "Rust"),
    "rust_old": ("Rust %s is too old: the port needs %s or newer.", "Rust %s é antiga demais: o port precisa da %s ou mais nova.",
                 "Rust %s es demasiado antiguo: el port necesita %s o más reciente.", "Rust %s est trop ancien : le portage demande %s ou plus récent."),
    "rust_none": ("Rust is not installed.", "Rust não está instalada.", "Rust no está instalado.", "Rust n'est pas installé."),
    "rustup_update": ("Update it with rustup (rustup default stable, rustup update stable)?",
                      "Atualizar com rustup (rustup default stable, rustup update stable)?",
                      "¿Actualizarlo con rustup (rustup default stable, rustup update stable)?",
                      "Le mettre à jour avec rustup (rustup default stable, rustup update stable) ?"),
    "rustup_install": ("Install current Rust with rustup (https://rustup.rs, into ~/.cargo; a Rust from your distribution stays installed but isn't used here)?",
                       "Instalar a Rust atual com rustup (https://rustup.rs, em ~/.cargo; a Rust da sua distribuição continua instalada, mas não é usada aqui)?",
                       "¿Instalar Rust actual con rustup (https://rustup.rs, en ~/.cargo; el Rust de tu distribución sigue instalado pero no se usa aquí)?",
                       "Installer Rust à jour avec rustup (https://rustup.rs, dans ~/.cargo ; le Rust de votre distribution reste installé mais n'est pas utilisé ici) ?"),
    "rust_needed": ("Rust %s or newer is needed.", "É preciso Rust %s ou mais nova.", "Hace falta Rust %s o más reciente.", "Rust %s ou plus récent est nécessaire."),
    "rust_still": ("still no Rust %s or newer (found: %s).", "ainda sem Rust %s ou mais nova (encontrada: %s).",
                   "todavía no hay Rust %s o más reciente (encontrado: %s).", "toujours pas de Rust %s ou plus récent (trouvé : %s)."),
    "copy_prog": ("The game's program", "O programa do jogo", "El programa del juego", "Le programme du jeu"),
    "bad_slus": ("game/SLUS_203.67 is not the NTSC-U release this port needs (SLUS-20367). Delete it and use a dump of that disc.",
                 "game/SLUS_203.67 não é a versão NTSC-U de que o port precisa (SLUS-20367). Apague-o e use uma cópia desse disco.",
                 "game/SLUS_203.67 no es la versión NTSC-U que necesita el port (SLUS-20367). Bórralo y usa una copia de ese disco.",
                 "game/SLUS_203.67 n'est pas la version NTSC-U dont le portage a besoin (SLUS-20367). Supprimez-le et utilisez une copie de ce disque."),
    "building": ("Generating the game code and building (a few minutes the first time)", "Gerando o código do jogo e compilando (alguns minutos na primeira vez)",
                 "Generando el código del juego y compilando (unos minutos la primera vez)", "Génération du code du jeu et compilation (quelques minutes la première fois)"),
    "pictures": ("Title and menu pictures", "Imagens de título e menu", "Imágenes de título y menú", "Images de titre et de menu"),
    "already": ("already done", "já feito", "ya hecho", "déjà fait"),
    "movies": ("Movies", "Vídeos", "Vídeos", "Vidéos"),
    "no_ffmpeg": ("skipped: no ffmpeg (the game skips its movies)", "pulado: sem ffmpeg (o jogo pula os vídeos)",
                  "omitido: no hay ffmpeg (el juego omite sus vídeos)", "ignoré : pas de ffmpeg (le jeu saute ses vidéos)"),
    "no_mpeg2": ("skipped: this ffmpeg can't decode the movies (MPEG-2), so the game skips them. Install a full ffmpeg and run this again.",
                 "pulado: este ffmpeg não decodifica os vídeos (MPEG-2), então o jogo os pula. Instale um ffmpeg completo e rode de novo.",
                 "omitido: este ffmpeg no decodifica los vídeos (MPEG-2), así que el juego los omite. Instala un ffmpeg completo y vuelve a ejecutarlo.",
                 "ignoré : ce ffmpeg ne décode pas les vidéos (MPEG-2), le jeu les saute donc. Installez un ffmpeg complet, puis relancez."),
    "crt": ("with the CRT look", "com o visual CRT", "con aspecto CRT", "avec le rendu CRT"),
    "no_crt": ("CRT look unavailable (needs ffmpeg with libplacebo and a GPU); converting the movies without it",
               "visual CRT indisponível (precisa de ffmpeg com libplacebo e uma GPU); convertendo os vídeos sem ele",
               "aspecto CRT no disponible (necesita ffmpeg con libplacebo y una GPU); convirtiendo los vídeos sin él",
               "rendu CRT indisponible (il faut ffmpeg avec libplacebo et un GPU) ; conversion des vidéos sans lui"),
    "movies_failed": ("some movies could not be converted (see install.log); the game skips those.",
                      "alguns vídeos não puderam ser convertidos (veja install.log); o jogo os pula.",
                      "algunos vídeos no se pudieron convertir (ver install.log); el juego los omite.",
                      "certaines vidéos n'ont pas pu être converties (voir install.log) ; le jeu les saute."),
    "installing": ("Installing", "Instalando", "Instalando", "Installation"),
    "opt_system": ("  1) Install to the system: %s, with Freekstyle in the app menu (Games); needs sudo",
                   "  1) Instalar no sistema: %s, com o Freekstyle no menu de aplicativos (Jogos); precisa de sudo",
                   "  1) Instalar en el sistema: %s, con Freekstyle en el menú de aplicaciones (Juegos); necesita sudo",
                   "  1) Installer dans le système : %s, avec Freekstyle dans le menu des applications (Jeux) ; demande sudo"),
    "opt_here": ("  2) Keep it in this folder", "  2) Deixar nesta pasta", "  2) Dejarlo en esta carpeta", "  2) Le garder dans ce dossier"),
    "choose": ("Choose [1]: ", "Escolha [1]: ", "Elige [1]: ", "Choix [1] : "),
    "copying_disc": ("copying the disc image...", "copiando a imagem do disco...", "copiando la imagen del disco...", "copie de l'image du disque..."),
    "done": ("Done in %s", "Pronto em %s", "Listo en %s", "Terminé en %s"),
    "done_system": ("Freekstyle is in your app menu under Games (also: freekstyle-launcher in a\nterminal). This folder isn't needed to play any more; ./uninstall.py\n(or /opt/freekstyle/uninstall.py) removes it and what this script added.",
                    "O Freekstyle está no menu de aplicativos, em Jogos (ou: freekstyle-launcher\nno terminal). Esta pasta não é mais necessária para jogar; ./uninstall.py\n(ou /opt/freekstyle/uninstall.py) remove o jogo e o que este script adicionou.",
                    "Freekstyle está en el menú de aplicaciones, en Juegos (o: freekstyle-launcher\nen una terminal). Esta carpeta ya no hace falta para jugar; ./uninstall.py\n(o /opt/freekstyle/uninstall.py) quita el juego y lo que añadió este script.",
                    "Freekstyle est dans le menu des applications, dans Jeux (ou : freekstyle-launcher\ndans un terminal). Ce dossier n'est plus nécessaire pour jouer ; ./uninstall.py\n(ou /opt/freekstyle/uninstall.py) supprime le jeu et ce que ce script a ajouté."),
    "done_here": ("Start the game with:   ./target/release/freekstyle-launcher", "Inicie o jogo com:   ./target/release/freekstyle-launcher",
                  "Inicia el juego con:   ./target/release/freekstyle-launcher", "Lancez le jeu avec :   ./target/release/freekstyle-launcher"),
    "failed_cmd": ("this command failed (exit %s): %s", "este comando falhou (saída %s): %s", "este comando falló (salida %s): %s", "cette commande a échoué (code %s) : %s"),
    "see_log": ("the full output is in %s", "a saída completa está em %s", "la salida completa está en %s", "la sortie complète est dans %s"),
    "interrupted": ("interrupted.", "interrompido.", "interrumpido.", "interrompu."),
}

def t(key, *args):
    text = MESSAGES[key][LANG]
    return text % args if args else text

# ---- Output ----

COLOUR = sys.stdout.isatty() and "NO_COLOR" not in os.environ

def paint(code, text):
    return "\033[%sm%s\033[0m" % (code, text) if COLOUR else text

class Failure(Exception):
    """A problem the user can fix; the message says how. `logged`: a
    command failed, and its output is in the log."""

    def __init__(self, message, logged=False):
        super().__init__(message)
        self.logged = logged

class Ui:
    def __init__(self, assume_yes, total):
        self.assume_yes = assume_yes
        self.total = total
        self.number = 0
        self.log = open(LOG, "a", encoding="utf-8")
        self.log.write("\n==== %s: %s\n" % (time.strftime("%Y-%m-%d %H:%M:%S"), " ".join(sys.argv)))

    def step(self, title):
        self.number += 1
        line = "[%d/%d] %s" % (self.number, self.total, title)
        print("\n" + paint("1", line))
        self.log.write("\n== %s\n" % line)

    def say(self, text):
        print("  " + text.replace("\n", "\n  "))
        self.log.write(text + "\n")

    def good(self, text):
        self.say(paint("32", "✓") + " " + text)

    def bad(self, text):
        self.say(paint("31", "✗") + " " + text)

    def warn(self, text):
        self.say(paint("33", t("warning")) + " " + text)

    def ask(self, question, default=True):
        hint = t("yes_no") if default else t("yes_no_no")
        prompt = "  %s %s " % (question.replace("\n", "\n  "), hint)
        if self.assume_yes:
            print(prompt + "y")
            return True
        try:
            reply = input(prompt).strip().lower()
        except EOFError:
            print()
            return False
        if not reply:
            return default
        return reply[0] in "yso"  # yes, sim/sí, oui

    def choose(self, prompt, default):
        if self.assume_yes:
            print(prompt + default)
            return default
        try:
            return input(prompt).strip() or default
        except EOFError:
            print()
            return "2"

    def run(self, cmd, sudo=False, quiet=False, check=True, attached=False, cwd=ROOT, env=None):
        """Run a command, showing and logging its output. `attached` gives it
        the terminal (package managers draw progress bars); `quiet` only
        logs. Raises Failure if it fails and `check` is set."""
        cmd = [str(c) for c in cmd]
        if sudo and os.geteuid() != 0:
            cmd = ["sudo"] + cmd
        shown = " ".join(cmd)
        if not quiet:
            print(paint("2", "  $ " + shown))
        self.log.write("$ %s\n" % shown)
        self.log.flush()
        if attached:
            code = subprocess.call(cmd, cwd=cwd, env=env)
        else:
            p = subprocess.Popen(cmd, cwd=cwd, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                 stdin=subprocess.DEVNULL, text=True, errors="replace", bufsize=1)
            for line in p.stdout:
                self.log.write(line)
                if not quiet:
                    sys.stdout.write("    " + line)
            code = p.wait()
        self.log.write("(exit %d)\n" % code)
        if code != 0 and check:
            raise Failure(t("failed_cmd", code, shown), logged=True)
        return code == 0

def have(name):
    return shutil.which(name) is not None

def output(cmd):
    """A command's output, or "" if it can't run."""
    try:
        return subprocess.run(cmd, capture_output=True, text=True, errors="replace").stdout
    except OSError:
        return ""

# ---- The machine ----

def distro_family():
    """arch, debian, fedora or suse, from /etc/os-release (ID, then
    ID_LIKE, so derivatives like Mint or Nobara count), else from which
    package manager exists."""
    fields = {}
    try:
        for line in Path("/etc/os-release").read_text().splitlines():
            if "=" in line:
                k, v = line.split("=", 1)
                fields[k] = v.strip().strip('"')
    except OSError:
        pass
    known = {"arch": "arch", "debian": "debian", "ubuntu": "debian", "fedora": "fedora", "rhel": "fedora",
             "suse": "suse", "opensuse": "suse"}
    for word in [fields.get("ID", "")] + fields.get("ID_LIKE", "").split():
        for name, family in known.items():
            if word == name or word.startswith(name + "-"):
                return family
    for pm, family in (("pacman", "arch"), ("apt-get", "debian"), ("dnf", "fedora"), ("zypper", "suse")):
        if have(pm):
            return family
    return None

def gpus():
    """GPU makers present: amd, intel, nvidia."""
    makers = set()
    for d in glob.glob("/sys/bus/pci/devices/*"):
        try:
            if not Path(d, "class").read_text().startswith("0x03"):
                continue
            vendor = Path(d, "vendor").read_text().strip()
        except OSError:
            continue
        makers.add({"0x1002": "amd", "0x8086": "intel", "0x10de": "nvidia"}.get(vendor))
    makers.discard(None)
    return makers

def vulkan_drivers(amd, intel, nouveau, nvidia, software):
    """The Vulkan driver packages for this machine's GPUs, for
    distributions that package them separately (the software one if none
    is recognised, as in most virtual machines)."""
    out = []
    for g in sorted(gpus()):
        if g == "amd":
            out.append(amd)
        elif g == "intel":
            out.append(intel)
        elif g == "nvidia":
            out.append(nvidia if Path("/sys/module/nvidia").exists() else nouveau)
    return out or [software]

def vulkan_ok():
    """The game draws with Vulkan: it needs the loader and a driver (ICD)."""
    icds = glob.glob("/usr/share/vulkan/icd.d/*.json") + glob.glob("/etc/vulkan/icd.d/*.json")
    loader = "libvulkan.so.1" in output(["ldconfig", "-p"]) or any(
        Path(p).exists() for p in ["/usr/lib/libvulkan.so.1", "/usr/lib64/libvulkan.so.1"] + glob.glob("/usr/lib/*-linux-gnu/libvulkan.so.1"))
    return bool(icds) and loader

def pkg_config(name):
    return have("pkg-config") and subprocess.call(["pkg-config", "--exists", name]) == 0

def ffmpeg_mpeg2():
    """The movies are MPEG-2, which some ffmpeg builds (Fedora's
    ffmpeg-free) leave out."""
    return re.search(r"^ V.{5} mpeg2video ", output(["ffmpeg", "-hide_banner", "-decoders"]), re.M) is not None

def pkg_installed(family, pkg):
    if family == "arch":
        return subprocess.call(["pacman", "-Q", pkg], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL) == 0
    if family == "debian":
        return "install ok installed" in output(["dpkg-query", "-W", "-f=${Status}", pkg])
    return subprocess.call(["rpm", "-q", pkg], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL) == 0

def installed_as(family, pkg):
    """The installed package that `pkg` resolved to: package managers also
    accept a name something else provides (zypper and dnf do), and only
    the real name can be removed later. None if nothing is installed."""
    if family in ("fedora", "suse"):
        out = output(["rpm", "-q", "--whatprovides", pkg, "--qf", "%{NAME}\\n"]).split()
        return out[0] if out and "no package" not in " ".join(out) else None
    return pkg if pkg_installed(family, pkg) else None

def install_command(family):
    """The package manager command, non-interactive (the user has already
    said yes)."""
    return {"arch": ["pacman", "-S", "--needed", "--noconfirm"], "debian": ["apt-get", "install", "-y"],
            "fedora": ["dnf", "install", "-y"], "suse": ["zypper", "--non-interactive", "install"]}[family]

def packages(family):
    """Packages for each requirement, per distribution."""
    if family == "arch":
        return {"compiler": ["base-devel"], "pkgconfig": ["pkgconf"], "glslc": ["shaderc"], "alsa": ["alsa-lib"],
                "udev": ["systemd-libs"], "ffmpeg": ["ffmpeg"], "curl": ["curl"],
                "vulkan": ["vulkan-icd-loader"] + vulkan_drivers("vulkan-radeon", "vulkan-intel", "vulkan-nouveau", "nvidia-utils", "vulkan-swrast")}
    if family == "debian":
        return {"compiler": ["build-essential"], "pkgconfig": ["pkg-config"], "glslc": ["glslc"], "alsa": ["libasound2-dev"],
                "udev": ["libudev-dev"], "ffmpeg": ["ffmpeg"], "curl": ["curl"], "vulkan": ["libvulkan1", "mesa-vulkan-drivers"]}
    if family == "fedora":
        return {"compiler": ["gcc"], "pkgconfig": ["pkgconf-pkg-config"], "glslc": ["glslc"], "alsa": ["alsa-lib-devel"],
                "udev": ["systemd-devel"], "ffmpeg": ["ffmpeg-free"], "curl": ["curl"], "vulkan": ["vulkan-loader", "mesa-vulkan-drivers"]}
    if family == "suse":
        return {"compiler": ["gcc"], "pkgconfig": ["pkgconf-pkg-config"], "glslc": ["shaderc"], "alsa": ["alsa-devel"],
                "udev": ["systemd-devel"], "ffmpeg": ["ffmpeg"], "curl": ["curl"],
                "vulkan": ["libvulkan1"] + vulkan_drivers("libvulkan_radeon", "libvulkan_intel", "libvulkan_lvp", "libvulkan_lvp", "libvulkan_lvp")}
    return {}

# ---- Rust ----

def version(text):
    m = re.search(r"(\d+)\.(\d+)(?:\.(\d+))?", text or "")
    return tuple(int(x or 0) for x in m.groups()) if m else None

def min_rust():
    m = re.search(r'^rust-version\s*=\s*"([^"]+)"', (ROOT / "Cargo.toml").read_text(), re.M)
    return m.group(1)

def rust_version():
    return version(output(["rustc", "--version"])) if have("rustc") and have("cargo") else None

def rust_ok():
    v = rust_version()
    return v is not None and v >= version(min_rust())

# ---- Steps ----

class Installer:
    def __init__(self, args):
        self.args = args
        self.ui = Ui(args.yes, 7)
        self.family = distro_family()
        try:
            self.state = json.loads(STATE.read_text())
        except (OSError, ValueError):
            self.state = {}
        self.state.update(source=str(ROOT), family=self.family)
        # rustup's Rust (~/.cargo/bin) goes before a distribution's, which
        # is often older than the dependencies need.
        cargo_bin = Path.home() / ".cargo" / "bin"
        if cargo_bin.is_dir():
            os.environ["PATH"] = "%s%s%s" % (cargo_bin, os.pathsep, os.environ["PATH"])

    def record(self, key, value):
        """Note something this script added, for uninstall.py. Lists
        (packages, files) grow; other keys are set."""
        if isinstance(value, list):
            have_ = self.state.setdefault(key, [])
            have_.extend(v for v in value if v not in have_)
        else:
            self.state[key] = value
        STATE.parent.mkdir(parents=True, exist_ok=True)
        STATE.write_text(json.dumps(self.state, indent=2))

    def cargo(self, *args, quiet=False):
        self.ui.run(["cargo"] + list(args), quiet=quiet)

    def dump(self):
        self.ui.step(t("look_dump"))
        GAME.mkdir(exist_ok=True)
        disc = GAME / "Freekstyle.bin"
        if disc.exists():
            self.ui.good(t("found_dump"))
        else:
            dumps = sorted(p for p in GAME.iterdir() if p.suffix.lower() in (".bin", ".iso") and p.is_file())
            if not dumps:
                raise Failure(t("no_dump"))
            if len(dumps) > 1:
                raise Failure(t("many_dumps", ", ".join(p.name for p in dumps)))
            if disc.is_symlink():
                disc.unlink()  # a dangling link from an earlier dump
            disc.symlink_to(dumps[0].name)
            self.record("files", ["game/Freekstyle.bin"])
            self.ui.good(t("using_dump", "game/" + dumps[0].name))
        free = shutil.disk_usage(ROOT).free // 10**9
        if free < SPACE_GB:
            self.ui.warn(t("space", free, SPACE_GB))
            if not self.ui.ask(t("continue"), default=False):
                sys.exit(1)

    def requirements(self):
        """(key, label, present, needed to build) for each requirement."""
        reqs = [
            ("compiler", t("req_compiler"), have("cc"), True),
            ("pkgconfig", t("req_pkgconfig"), have("pkg-config"), True),
            ("glslc", t("req_glslc"), have("glslc"), True),
            ("alsa", t("req_alsa"), pkg_config("alsa"), True),
            ("udev", t("req_udev"), pkg_config("libudev"), True),
            ("vulkan", t("req_vulkan"), vulkan_ok(), True),
            ("ffmpeg", t("req_ffmpeg"), have("ffmpeg"), False),
        ]
        # rustup is fetched with curl.
        if not rust_ok() and not have("rustup"):
            reqs.append(("curl", t("req_curl"), have("curl"), True))
        return reqs

    def tools(self):
        self.ui.step(t("check_tools"))
        reqs = self.requirements()
        for _, label, present, _ in reqs:
            (self.ui.good if present else self.ui.bad)(label)
        missing = [r for r in reqs if not r[2]]
        if missing:
            names = packages(self.family)
            if not names:
                raise Failure(t("missing_nopm", ", ".join(r[0] for r in missing)))
            pkgs = [p for key, _, _, _ in missing for p in names[key]]
            command = install_command(self.family) + pkgs
            if self.ui.ask(t("install_now", "sudo " + " ".join(command))):
                # Only packages that weren't there before are ours to remove.
                new = [p for p in pkgs if not installed_as(self.family, p)]
                if self.family == "debian":
                    # Package lists can be missing or stale on a fresh system.
                    self.ui.run(["apt-get", "update"], sudo=True, attached=True)
                self.ui.run(command, sudo=True, attached=True)
                self.record("packages", [n for n in (installed_as(self.family, p) for p in new) if n])
                left = [label for key, label, present, needed in self.requirements() if not present and needed]
                if left:
                    raise Failure(t("still_missing", ", ".join(left)))
            elif any(needed for _, _, _, needed in missing):
                raise Failure(t("cant_build"))
        if self.family == "fedora" and have("ffmpeg") and not ffmpeg_mpeg2():
            self.rpmfusion()
        self.rust()

    def rpmfusion(self):
        if not self.ui.ask(t("rpmfusion")):
            return
        release = output(["rpm", "-E", "%fedora"]).strip()
        self.ui.run(["dnf", "install", "-y",
                     "https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-%s.noarch.rpm" % release],
                    sudo=True, attached=True)
        self.record("rpmfusion", True)
        self.ui.run(["dnf", "swap", "-y", "ffmpeg-free", "ffmpeg", "--allowerasing"], sudo=True, attached=True)

    def rust(self):
        if not rust_ok():
            current = rust_version()
            if current:
                self.ui.say(t("rust_old", ".".join(map(str, current)), min_rust()))
            else:
                self.ui.say(t("rust_none"))
            if have("rustup"):
                if not self.ui.ask(t("rustup_update")):
                    raise Failure(t("rust_needed", min_rust()))
                self.ui.run(["rustup", "default", "stable"])
                self.ui.run(["rustup", "update", "stable"])
            elif self.ui.ask(t("rustup_install")):
                script = output(["curl", "--proto", "=https", "--tlsv1.2", "-sSf", "https://sh.rustup.rs"])
                if not script.startswith("#!"):
                    raise Failure(t("rust_needed", min_rust()))
                rustup_init = ROOT / "target" / "rustup-init.sh"
                rustup_init.parent.mkdir(exist_ok=True)
                rustup_init.write_text(script)
                self.ui.run(["sh", rustup_init, "-y", "--profile", "minimal"])
                self.record("rustup", True)
                os.environ["PATH"] = "%s%s%s" % (Path.home() / ".cargo" / "bin", os.pathsep, os.environ["PATH"])
            else:
                raise Failure(t("rust_needed", min_rust()))
            if not rust_ok():
                raise Failure(t("rust_still", min_rust(), output(["rustc", "--version"]).strip() or "-"))
        self.ui.good("%s: %s" % (t("rust"), output(["rustc", "--version"]).strip()))

    def program(self):
        self.ui.step(t("copy_prog"))
        slus = GAME / "SLUS_203.67"
        if not slus.is_file():
            self.cargo("run", "--release", "-q", "-p", "ee-runtime", "--bin", "disc-extract")
            self.record("files", ["game/SLUS_203.67"])
        if hashlib.sha256(slus.read_bytes()).hexdigest() != SLUS_SHA256:
            raise Failure(t("bad_slus"))
        self.ui.good("game/SLUS_203.67 (SLUS-20367)")

    def build(self):
        self.ui.step(t("building"))
        self.cargo("run", "--release", "-q", "-p", "ee-recomp", "--", "game/SLUS_203.67", "freekstyle/src/gen",
                   "freekstyle/hle.txt", "freekstyle/seeds.txt", "freekstyle/vu_dumps")
        self.cargo("build", "--release", "-p", "freekstyle", "-p", "freekstyle-launcher", "-p", "flm")

    def pictures(self):
        self.ui.step(t("pictures"))
        if glob.glob(str(GAME / "images" / "*.png")):
            self.ui.good(t("already"))
        else:
            self.cargo("run", "--release", "-q", "-p", "ee-runtime", "--bin", "lsc2png")
            self.record("files", ["game/images"])

    def movies(self):
        self.ui.step(t("movies"))
        movies = GAME / "movies"
        if not have("ffmpeg"):
            self.ui.warn(t("no_ffmpeg"))
            return
        if (movies / "hd" / "intro.mkv").is_file() or (movies / "intro.mkv").is_file():
            self.ui.good(t("already"))
            return
        if not ffmpeg_mpeg2():
            self.ui.warn(t("no_mpeg2"))
            return
        flm = movies / "flm"
        # Only what this step creates is recorded: game/movies may also hold
        # movies made by hand.
        before = {p for p in (flm, movies / "hd") if p.exists()}
        before |= set(movies.glob("*.mkv")) | set(movies.glob("hd/*.mkv"))
        try:
            self.convert_movies(movies, flm)
        finally:
            made = [p for p in [flm, movies / "hd"] + list(movies.glob("*.mkv")) if p.exists() and p not in before]
            made += [p for p in movies.glob("hd/*.mkv") if p not in before and (movies / "hd") in before]
            self.record("files", [str(p.relative_to(ROOT)) for p in made])

    def convert_movies(self, movies, flm):
        for m in MOVIES:
            folder, name = m.split("/")
            if not (flm / (name + ".FLM")).is_file():
                self.cargo("run", "--release", "-q", "-p", "ee-runtime", "--bin", "disc-extract", "--",
                           "game/Freekstyle.bin", "\\SXDATA\\%s\\%s.FLM" % (folder, name), flm, quiet=True)
        flm2mkv = ROOT / "target" / "release" / "flm2mkv"
        # The CRT look needs ffmpeg's libplacebo filter and a Vulkan GPU (not
        # a software one, as in most virtual machines): try a tiny clip first.
        crt = self.ui.run(["ffmpeg", "-hide_banner", "-loglevel", "quiet", "-f", "lavfi", "-i", "color=size=64x64:duration=0.1",
                           "-vf", "libplacebo", "-f", "null", "-"], quiet=True, check=False)
        if crt and self.ui.run([flm2mkv, "--crt", flm, movies / "hd"], check=False):
            self.ui.good(t("crt"))
            return
        self.ui.warn(t("no_crt"))
        shutil.rmtree(movies / "hd", ignore_errors=True)
        if not self.ui.run([flm2mkv, flm, movies], check=False):
            self.ui.warn(t("movies_failed"))

    def install(self):
        self.ui.step(t("installing"))
        if self.args.system:
            where = "1"
        elif self.args.here:
            where = "2"
        else:
            self.ui.say(t("opt_system", PREFIX).strip())
            self.ui.say(t("opt_here").strip())
            where = self.ui.choose("  " + t("choose"), "1")
        if where != "1":
            return False
        run = self.ui.run
        release = ROOT / "target" / "release"
        run(["rm", "-rf", PREFIX / "game"], sudo=True)
        run(["mkdir", "-p", PREFIX / "game" / "movies"], sudo=True)
        run(["install", "-m", "755", release / "freekstyle", release / "freekstyle-launcher", PREFIX], sudo=True)
        run(["install", "-m", "644", GAME / "SLUS_203.67", PREFIX / "game"], sudo=True)
        self.ui.say(t("copying_disc"))
        run(["cp", "-L", "--reflink=auto", GAME / "Freekstyle.bin", PREFIX / "game" / "Freekstyle.bin"], sudo=True)
        if (GAME / "images").is_dir():
            run(["cp", "-r", GAME / "images", PREFIX / "game"], sudo=True)
        if (GAME / "movies" / "hd").is_dir():
            run(["cp", "-r", GAME / "movies" / "hd", PREFIX / "game" / "movies"], sudo=True)
        plain = glob.glob(str(GAME / "movies" / "*.mkv"))
        if plain:
            run(["cp"] + plain + [PREFIX / "game" / "movies"], sudo=True)
        # The uninstaller goes along, for when this folder is gone.
        run(["install", "-m", "755", ROOT / "uninstall.py", PREFIX], sudo=True)
        run(["chmod", "-R", "a+rX", PREFIX], sudo=True)
        run(["ln", "-sf", PREFIX / "freekstyle-launcher", "/usr/local/bin/freekstyle-launcher"], sudo=True)
        # Run from the install, so the entries point there (not this folder).
        run(["./freekstyle-launcher", "--install-menu", "/usr/share"], sudo=True, cwd=PREFIX)
        # Entries from running the launcher in this folder would hide the
        # system one.
        data = Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local" / "share")
        for name in ("freekstyle.desktop", "freekstyle-launcher.desktop"):
            (data / "applications" / name).unlink(missing_ok=True)
        for icon in glob.glob(str(data / "icons" / "hicolor" / "*" / "apps" / "freekstyle*.png")):
            os.remove(icon)
        # Settings made here carry over (the install directory is read-only).
        config = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config") / "freekstyle"
        if (ROOT / "freekstyle.toml").is_file() and not (config / "freekstyle.toml").exists():
            config.mkdir(parents=True, exist_ok=True)
            shutil.copy(ROOT / "freekstyle.toml", config)
        refresh_menus(self.ui)
        self.record("system", str(PREFIX))
        return True

def refresh_menus(ui):
    """Let the desktops see menu and icon changes now, not at next login."""
    if have("update-desktop-database"):
        ui.run(["update-desktop-database", "-q", "/usr/share/applications"], sudo=True, quiet=True, check=False)
    if have("gtk-update-icon-cache"):
        ui.run(["gtk-update-icon-cache", "-q", "-t", "-f", "/usr/share/icons/hicolor"], sudo=True, quiet=True, check=False)
    if have("kbuildsycoca6"):
        ui.run(["kbuildsycoca6"], quiet=True, check=False)

def main():
    parser = argparse.ArgumentParser(description="Set up Freekstyle from your own disc dump (see README.md).")
    parser.add_argument("--uninstall", action="store_true", help="run uninstall.py (removes the game and what this script added)")
    parser.add_argument("-y", "--yes", action="store_true", help="answer yes to every question (installs packages and Rust without asking)")
    where = parser.add_mutually_exclusive_group()
    where.add_argument("--system", action="store_true", help="install to %s at the end without asking" % PREFIX)
    where.add_argument("--here", action="store_true", help="keep the game in this folder at the end without asking")
    args = parser.parse_args()

    if os.geteuid() == 0:
        print(paint("31", t("error")), t("root"), file=sys.stderr)
        return 1
    if args.uninstall:
        uninstaller = ROOT / "uninstall.py"
        os.execv(sys.executable, [sys.executable, str(uninstaller)] + (["--yes"] if args.yes else []))
    start = time.monotonic()
    installer = None
    ui = None
    try:
        installer = Installer(args)
        ui = installer.ui
        for step in (installer.dump, installer.tools, installer.program, installer.build, installer.pictures, installer.movies):
            step()
        system = installer.install()
    except Failure as e:
        print("\n" + paint("31", t("error")), e, file=sys.stderr)
        if ui:
            ui.log.write("error: %s\n" % e)
            if e.logged:
                print("  " + t("see_log", LOG), file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("\n" + t("interrupted"), file=sys.stderr)
        return 130
    minutes, seconds = divmod(int(time.monotonic() - start), 60)
    print("\n" + paint("1;32", t("done", "%dm%02ds" % (minutes, seconds))))
    print(t("done_system") if system else t("done_here"))
    print()
    return 0

if __name__ == "__main__":
    sys.exit(main())
