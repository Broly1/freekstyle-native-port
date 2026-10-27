//! Launcher text in English, Brazilian Portuguese, Spanish and French.
//! `{}` in a string is replaced by the caller (see [`fill`]).

#[derive(Clone, Copy, PartialEq)]
pub enum Lang {
    En,
    PtBr,
    Es,
    Fr,
}

impl Lang {
    pub const ALL: [Lang; 4] = [Lang::En, Lang::PtBr, Lang::Es, Lang::Fr];

    /// The code saved in freekstyle.toml.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::PtBr => "pt-BR",
            Lang::Es => "es",
            Lang::Fr => "fr",
        }
    }

    /// The language's name in itself, for the picker.
    pub fn name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::PtBr => "Português (Brasil)",
            Lang::Es => "Español",
            Lang::Fr => "Français",
        }
    }

    /// The saved code, or the system's language when it is empty or unknown.
    pub fn from_setting(code: &str) -> Lang {
        Lang::ALL.into_iter().find(|l| l.code() == code).unwrap_or_else(Lang::system)
    }

    /// From the locale variables, in the order gettext reads them.
    pub fn system() -> Lang {
        let locale = ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|v| std::env::var(v).ok())
            .find(|v| !v.is_empty())
            .unwrap_or_default();
        match locale.get(..2) {
            Some("pt") => Lang::PtBr,
            Some("es") => Lang::Es,
            Some("fr") => Lang::Fr,
            _ => Lang::En,
        }
    }

    pub fn tr(self) -> &'static Tr {
        match self {
            Lang::En => &EN,
            Lang::PtBr => &PT_BR,
            Lang::Es => &ES,
            Lang::Fr => &FR,
        }
    }
}

/// Replace each `{}` in `pattern` with the next of `args`.
pub fn fill(pattern: &str, args: &[&str]) -> String {
    let mut out = String::new();
    let mut parts = pattern.split("{}");
    out.push_str(parts.next().unwrap_or(""));
    for (part, arg) in parts.zip(args.iter().chain(std::iter::repeat(&""))) {
        out.push_str(arg);
        out.push_str(part);
    }
    out
}

pub struct Tr {
    pub subtitle: &'static str,
    pub play: &'static str,
    pub running: &'static str,
    pub settings_tab: &'static str,
    pub controls_tab: &'static str,
    pub saved_to: &'static str,

    pub display: &'static str,
    pub resolution: &'static str,
    pub internal_resolution: &'static str,
    pub match_window: &'static str,
    pub fullscreen: &'static str,
    pub fullscreen_hint: &'static str,
    pub widescreen: &'static str,
    pub widescreen_hint: &'static str,
    pub stretch: &'static str,
    pub stretch_hint: &'static str,
    pub side_bars: &'static str,
    pub side_bars_hint: &'static str,
    pub sharp_textures: &'static str,
    pub sharp_textures_hint: &'static str,

    pub game: &'static str,
    pub sound: &'static str,
    pub mute: &'static str,
    pub button_icons: &'static str,
    pub ps2_icons_hint: &'static str,
    pub disc_image: &'static str,
    pub executable: &'static str,
    pub language: &'static str,
    pub system_language: &'static str,

    pub controller_named: &'static str,
    pub no_controller: &'static str,
    pub reset_defaults: &'static str,
    pub controls_help: &'static str,
    pub ps2_button: &'static str,
    pub keyboard: &'static str,
    pub controller: &'static str,
    pub click_to_remove: &'static str,
    pub press_key: &'static str,
    pub press_button: &'static str,
    pub add_binding: &'static str,

    pub game_running: &'static str,
    pub game_closed: &'static str,
    pub game_exited: &'static str,
    pub build_hint: &'static str,
    pub moved_from: &'static str,
    pub reserved_key: &'static str,
    pub cant_bind: &'static str,
    pub could_not_save: &'static str,
    pub using_defaults: &'static str,

    /// Labels for freek_config::PS2_BUTTONS, in its order.
    pub buttons: [&'static str; 16],
    pub dpad: &'static str,
    pub ls_click: &'static str,
    pub rs_click: &'static str,

    /// Desktop entry comments for the game and the launcher.
    pub game_comment: &'static str,
    pub launcher_comment: &'static str,
}

pub const EN: Tr = Tr {
    subtitle: "Launcher · native PC port",
    play: "▶  Play",
    running: "Running…",
    settings_tab: "Settings",
    controls_tab: "Controls",
    saved_to: "Saved to {}",

    display: "Display",
    resolution: "Resolution",
    internal_resolution: "Internal resolution",
    match_window: "Match window",
    fullscreen: "Fullscreen",
    fullscreen_hint: "Borderless fullscreen (F11 in game)",
    widescreen: "Widescreen",
    widescreen_hint: "16:9 view with wide pictures",
    stretch: "Stretch",
    stretch_hint: "Fill the window instead of keeping the shape",
    side_bars: "Side bars",
    side_bars_hint: "Blurred picture instead of black bars",
    sharp_textures: "Sharp textures",
    sharp_textures_hint: "Upscale menu text and artwork (xBRZ)",

    game: "Game",
    sound: "Sound",
    mute: "Mute",
    button_icons: "Button icons",
    ps2_icons_hint: "Keep the original PlayStation icons",
    disc_image: "Disc image",
    executable: "Executable",
    language: "Language",
    system_language: "System language",

    controller_named: "Controller: {}",
    no_controller: "No controller connected",
    reset_defaults: "Reset to defaults",
    controls_help: "Click + and press a key or controller button. Click a binding to remove it. Keyboard and controller work together.",
    ps2_button: "PS2 button",
    keyboard: "Keyboard",
    controller: "Controller",
    click_to_remove: "Click to remove",
    press_key: "Press a key… (Esc cancels)",
    press_button: "Press a button…",
    add_binding: "Add a binding",

    game_running: "Game running",
    game_closed: "Game closed",
    game_exited: "Game exited: {}",
    build_hint: "build it with cargo build --release -p freekstyle",
    moved_from: "{} moved from {}",
    reserved_key: "{} is reserved by the game",
    cant_bind: "That key can't be bound",
    could_not_save: "could not save: {}",
    using_defaults: "{}; using defaults",

    buttons: [
        "A (Cross)",
        "B (Circle)",
        "X (Square)",
        "Y (Triangle)",
        "LB (L1)",
        "RB (R1)",
        "LT (L2)",
        "RT (R2)",
        "Left stick click (L3)",
        "Right stick click (R3)",
        "Start",
        "Back (Select)",
        "D-pad up",
        "D-pad down",
        "D-pad left",
        "D-pad right",
    ],
    dpad: "D-pad",
    ls_click: "LS click",
    rs_click: "RS click",

    game_comment: "Freekstyle, native port",
    launcher_comment: "Freekstyle display settings and controls",
};

pub const PT_BR: Tr = Tr {
    subtitle: "Inicializador · port nativo para PC",
    play: "▶  Jogar",
    running: "Rodando…",
    settings_tab: "Configurações",
    controls_tab: "Controles",
    saved_to: "Salvo em {}",

    display: "Vídeo",
    resolution: "Resolução",
    internal_resolution: "Resolução interna",
    match_window: "Igual à janela",
    fullscreen: "Tela cheia",
    fullscreen_hint: "Tela cheia sem bordas (F11 no jogo)",
    widescreen: "Widescreen",
    widescreen_hint: "Visão 16:9 com imagens largas",
    stretch: "Esticar",
    stretch_hint: "Preencher a janela em vez de manter a proporção",
    side_bars: "Barras laterais",
    side_bars_hint: "Imagem desfocada em vez de barras pretas",
    sharp_textures: "Texturas nítidas",
    sharp_textures_hint: "Ampliar textos e artes dos menus (xBRZ)",

    game: "Jogo",
    sound: "Som",
    mute: "Sem som",
    button_icons: "Ícones dos botões",
    ps2_icons_hint: "Manter os ícones originais do PlayStation",
    disc_image: "Imagem do disco",
    executable: "Executável",
    language: "Idioma",
    system_language: "Idioma do sistema",

    controller_named: "Controle: {}",
    no_controller: "Nenhum controle conectado",
    reset_defaults: "Restaurar padrões",
    controls_help: "Clique em + e pressione uma tecla ou um botão do controle. Clique em uma atribuição para removê-la. Teclado e controle funcionam juntos.",
    ps2_button: "Botão do PS2",
    keyboard: "Teclado",
    controller: "Controle",
    click_to_remove: "Clique para remover",
    press_key: "Pressione uma tecla… (Esc cancela)",
    press_button: "Pressione um botão…",
    add_binding: "Adicionar atribuição",

    game_running: "Jogo em execução",
    game_closed: "Jogo fechado",
    game_exited: "O jogo saiu: {}",
    build_hint: "compile com cargo build --release -p freekstyle",
    moved_from: "{} saiu de {}",
    reserved_key: "{} é reservada pelo jogo",
    cant_bind: "Essa tecla não pode ser atribuída",
    could_not_save: "não foi possível salvar: {}",
    using_defaults: "{}; usando os padrões",

    buttons: [
        "A (Xis)",
        "B (Bola)",
        "X (Quadrado)",
        "Y (Triângulo)",
        "LB (L1)",
        "RB (R1)",
        "LT (L2)",
        "RT (R2)",
        "Clique do analógico esquerdo (L3)",
        "Clique do analógico direito (R3)",
        "Start",
        "Back (Select)",
        "Direcional para cima",
        "Direcional para baixo",
        "Direcional para a esquerda",
        "Direcional para a direita",
    ],
    dpad: "Direcional",
    ls_click: "Clique LS",
    rs_click: "Clique RS",

    game_comment: "Freekstyle, port nativo",
    launcher_comment: "Vídeo e controles do Freekstyle",
};

pub const ES: Tr = Tr {
    subtitle: "Lanzador · port nativo para PC",
    play: "▶  Jugar",
    running: "En ejecución…",
    settings_tab: "Ajustes",
    controls_tab: "Controles",
    saved_to: "Guardado en {}",

    display: "Pantalla",
    resolution: "Resolución",
    internal_resolution: "Resolución interna",
    match_window: "Igual que la ventana",
    fullscreen: "Pantalla completa",
    fullscreen_hint: "Pantalla completa sin bordes (F11 en el juego)",
    widescreen: "Panorámico",
    widescreen_hint: "Vista 16:9 con imágenes anchas",
    stretch: "Estirar",
    stretch_hint: "Llenar la ventana en lugar de mantener la proporción",
    side_bars: "Barras laterales",
    side_bars_hint: "Imagen difuminada en lugar de barras negras",
    sharp_textures: "Texturas nítidas",
    sharp_textures_hint: "Escalar textos e ilustraciones de los menús (xBRZ)",

    game: "Juego",
    sound: "Sonido",
    mute: "Silenciar",
    button_icons: "Iconos de botones",
    ps2_icons_hint: "Mantener los iconos originales de PlayStation",
    disc_image: "Imagen del disco",
    executable: "Ejecutable",
    language: "Idioma",
    system_language: "Idioma del sistema",

    controller_named: "Mando: {}",
    no_controller: "Ningún mando conectado",
    reset_defaults: "Restablecer valores predeterminados",
    controls_help: "Haz clic en + y pulsa una tecla o un botón del mando. Haz clic en una asignación para quitarla. El teclado y el mando funcionan a la vez.",
    ps2_button: "Botón de PS2",
    keyboard: "Teclado",
    controller: "Mando",
    click_to_remove: "Clic para quitar",
    press_key: "Pulsa una tecla… (Esc cancela)",
    press_button: "Pulsa un botón…",
    add_binding: "Añadir asignación",

    game_running: "Juego en ejecución",
    game_closed: "Juego cerrado",
    game_exited: "El juego terminó: {}",
    build_hint: "compílalo con cargo build --release -p freekstyle",
    moved_from: "{} se quitó de {}",
    reserved_key: "{} está reservada por el juego",
    cant_bind: "Esa tecla no se puede asignar",
    could_not_save: "no se pudo guardar: {}",
    using_defaults: "{}; se usan los valores predeterminados",

    buttons: [
        "A (Equis)",
        "B (Círculo)",
        "X (Cuadrado)",
        "Y (Triángulo)",
        "LB (L1)",
        "RB (R1)",
        "LT (L2)",
        "RT (R2)",
        "Clic del stick izquierdo (L3)",
        "Clic del stick derecho (R3)",
        "Start",
        "Back (Select)",
        "Cruceta arriba",
        "Cruceta abajo",
        "Cruceta izquierda",
        "Cruceta derecha",
    ],
    dpad: "Cruceta",
    ls_click: "Clic LS",
    rs_click: "Clic RS",

    game_comment: "Freekstyle, port nativo",
    launcher_comment: "Pantalla y controles de Freekstyle",
};

pub const FR: Tr = Tr {
    subtitle: "Lanceur · portage PC natif",
    play: "▶  Jouer",
    running: "En cours…",
    settings_tab: "Paramètres",
    controls_tab: "Commandes",
    saved_to: "Enregistré dans {}",

    display: "Affichage",
    resolution: "Résolution",
    internal_resolution: "Résolution interne",
    match_window: "Comme la fenêtre",
    fullscreen: "Plein écran",
    fullscreen_hint: "Plein écran sans bordure (F11 en jeu)",
    widescreen: "Écran large",
    widescreen_hint: "Vue 16:9 avec images larges",
    stretch: "Étirer",
    stretch_hint: "Remplir la fenêtre au lieu de garder les proportions",
    side_bars: "Bandes latérales",
    side_bars_hint: "Image floutée au lieu de bandes noires",
    sharp_textures: "Textures nettes",
    sharp_textures_hint: "Agrandir les textes et illustrations des menus (xBRZ)",

    game: "Jeu",
    sound: "Son",
    mute: "Couper le son",
    button_icons: "Icônes des boutons",
    ps2_icons_hint: "Garder les icônes PlayStation d'origine",
    disc_image: "Image disque",
    executable: "Exécutable",
    language: "Langue",
    system_language: "Langue du système",

    controller_named: "Manette : {}",
    no_controller: "Aucune manette connectée",
    reset_defaults: "Rétablir les valeurs par défaut",
    controls_help: "Cliquez sur + puis appuyez sur une touche ou un bouton de la manette. Cliquez sur une affectation pour la supprimer. Clavier et manette fonctionnent ensemble.",
    ps2_button: "Bouton PS2",
    keyboard: "Clavier",
    controller: "Manette",
    click_to_remove: "Cliquer pour supprimer",
    press_key: "Appuyez sur une touche… (Échap annule)",
    press_button: "Appuyez sur un bouton…",
    add_binding: "Ajouter une affectation",

    game_running: "Jeu en cours",
    game_closed: "Jeu fermé",
    game_exited: "Le jeu s'est arrêté : {}",
    build_hint: "compilez-le avec cargo build --release -p freekstyle",
    moved_from: "{} retirée de {}",
    reserved_key: "{} est réservée par le jeu",
    cant_bind: "Cette touche ne peut pas être affectée",
    could_not_save: "impossible d'enregistrer : {}",
    using_defaults: "{} ; valeurs par défaut utilisées",

    buttons: [
        "A (Croix)",
        "B (Rond)",
        "X (Carré)",
        "Y (Triangle)",
        "LB (L1)",
        "RB (R1)",
        "LT (L2)",
        "RT (R2)",
        "Clic du stick gauche (L3)",
        "Clic du stick droit (R3)",
        "Start",
        "Back (Select)",
        "Croix directionnelle haut",
        "Croix directionnelle bas",
        "Croix directionnelle gauche",
        "Croix directionnelle droite",
    ],
    dpad: "Croix",
    ls_click: "Clic LS",
    rs_click: "Clic RS",

    game_comment: "Freekstyle, portage natif",
    launcher_comment: "Affichage et commandes de Freekstyle",
};
