//! k535-gui - interfaz grafica (GTK4) para el teclado Redragon K535-KB.
//!
//! Toda la comunicacion con el HID ocurre en un hilo aparte; la interfaz
//! solo recibe eventos por un canal y nunca se bloquea.
//!
//! Estructura:
//!   - cabecera con el estado de conexion
//!   - selector de perfil (3 perfiles del firmware)
//!   - iluminacion: efecto, brillo, velocidad, encendido
//!   - color: selector global + "aplicar a todas las teclas"
//!   - teclado dibujado con cairo: clic en una tecla para colorearla

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use gtk::cairo;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

use k535::{Device, State, KEY_COUNT, KEYMAP, MODE_NAMES, NPROFILES};

const APP_ID: &str = "com.github.Gjaset.k535";
const REFRESH_MS: u64 = 120;
const RECONNECT_MS: u64 = 1500;

const CSS: &str = r#"
.card {
    background-color: alpha(@theme_fg_color, 0.045);
    border-radius: 16px;
    padding: 18px 20px;
}
.section-title {
    font-weight: bold;
    font-size: 105%;
    opacity: 0.85;
}
.caption {
    font-size: 88%;
    opacity: 0.55;
}
.big-title {
    font-size: 150%;
    font-weight: bold;
}
.statusbar {
    padding: 10px 22px;
    font-size: 90%;
    opacity: 0.75;
}
.badge {
    border-radius: 10px;
    padding: 3px 12px;
    font-size: 88%;
}
.badge-ok {
    background-color: alpha(@success_color, 0.22);
}
.badge-ko {
    background-color: alpha(@error_color, 0.22);
}
.flat-button {
    padding: 6px 14px;
}
"#;

// ----------------------------------------------------------------- canal

enum Cmd {
    Refresh,
    Activate(u8),
    ReadKeys(u8),
    Field {
        profile: u8,
        offset: u8,
        value: u8,
        label: &'static str,
    },
    ProfileColor {
        profile: u8,
        rgb: [u8; 3],
    },
    KeyColor {
        profile: u8,
        index: u16,
        rgb: [u8; 3],
    },
    AllKeys {
        profile: u8,
        rgb: [u8; 3],
    },
}

enum Evt {
    State(Box<State>),
    Keys(Box<HashMap<u16, [u8; 3]>>),
    Error(String),
    Note(String),
}

// --------------------------------------------------------------- worker

fn worker_loop(rx: mpsc::Receiver<Cmd>, tx: mpsc::Sender<Evt>) {
    let mut dev: Option<Device> = None;
    let mut state = State::default();

    loop {
        let cmd = match rx.recv() {
            Ok(c) => c,
            Err(_) => break,
        };

        if dev.is_none() {
            match Device::open() {
                Ok(d) => dev = Some(d),
                Err(e) => {
                    state.connected = false;
                    let _ = tx.send(Evt::Error(format!(
                        "{e}\nsudo udevadm trigger --subsystem-match=hidraw"
                    )));
                    continue;
                }
            }
        }
        let d = dev.as_mut().unwrap();

        let res: Result<(), String> = match cmd {
            Cmd::Refresh => d
                .read_state()
                .map(|s| {
                    state.profiles = s.profiles;
                    state.active = s.active;
                    state.connected = true;
                    let _ = tx.send(Evt::State(Box::new(state.clone())));
                })
                .map_err(|e| e.to_string()),

            Cmd::Activate(p) => d
                .activate_profile(p)
                .and_then(|_| d.read_state())
                .map(|s| {
                    state.profiles = s.profiles;
                    state.active = s.active;
                    state.connected = true;
                    let _ = tx.send(Evt::State(Box::new(state.clone())));
                })
                .map_err(|e| e.to_string()),

            // Lee el mapa de colores por tecla (mejor effort: si el firmware
            // no lo permite, se ignora el error).
            Cmd::ReadKeys(p) => {
                if let Ok(map) = d.read_key_colors(p) {
                    let _ = tx.send(Evt::Keys(Box::new(map)));
                }
                Ok(())
            }

            Cmd::Field {
                profile,
                offset,
                value,
                label,
            } => d
                .set_profile_field(profile, offset, value)
                .and_then(|_| d.activate_profile(profile))
                .and_then(|_| d.read_state())
                .map(|s| {
                    state.profiles = s.profiles;
                    state.active = s.active;
                    let _ = tx.send(Evt::State(Box::new(state.clone())));
                    let _ = tx.send(Evt::Note(format!("perfil {profile}: {label} = {value}")));
                })
                .map_err(|e| e.to_string()),

            Cmd::ProfileColor { profile, rgb } => d
                .set_profile_color(profile, rgb)
                .and_then(|_| d.activate_profile(profile))
                .and_then(|_| d.read_state())
                .map(|s| {
                    state.profiles = s.profiles;
                    state.active = s.active;
                    let _ = tx.send(Evt::State(Box::new(state.clone())));
                    let _ = tx.send(Evt::Note(format!(
                        "perfil {profile}: color {},{},{}",
                        rgb[0], rgb[1], rgb[2]
                    )));
                })
                .map_err(|e| e.to_string()),

            Cmd::KeyColor {
                profile,
                index,
                rgb,
            } => d
                .set_key_color(profile, index, rgb)
                .map(|_| {
                    state.key_colors.insert((profile, index), rgb);
                    let _ = tx.send(Evt::State(Box::new(state.clone())));
                    let _ = tx.send(Evt::Note(format!(
                        "tecla {index} = {},{},{}",
                        rgb[0], rgb[1], rgb[2]
                    )));
                })
                .map_err(|e| e.to_string()),

            Cmd::AllKeys { profile, rgb } => {
                let items: Vec<(u16, [u8; 3])> = (0..KEY_COUNT).map(|i| (i, rgb)).collect();
                d.set_key_colors_bulk(profile, &items)
                    .map(|_| {
                        for i in 0..KEY_COUNT {
                            state.key_colors.insert((profile, i), rgb);
                        }
                        let _ = tx.send(Evt::State(Box::new(state.clone())));
                        let _ = tx.send(Evt::Note(format!(
                            "{} teclas en {},{},{}",
                            items.len(),
                            rgb[0],
                            rgb[1],
                            rgb[2]
                        )));
                    })
                    .map_err(|e| e.to_string())
            }
        };

        if let Err(msg) = res {
            state.connected = false;
            dev = None;
            let _ = tx.send(Evt::Error(msg));
        }
    }
}

// ------------------------------------------------------------------ UI

#[derive(Clone)]
struct Shared {
    tx: mpsc::Sender<Cmd>,
    profile: Rc<Cell<u8>>,
    selected_color: Rc<RefCell<[u8; 3]>>,
    connected: Rc<Cell<bool>>,
    /// Perfil del que se leyo por ultima vez el mapa de colores.
    keys_profile: Rc<Cell<i32>>,
}

/// Una tecla del teclado: indice del dispositivo, etiqueta y anchura (en unidades).
#[derive(Clone, Copy)]
struct Key {
    /// Indice del dispositivo (None = tecla dibujada pero sin calibrar).
    index: Option<u16>,
    label: &'static str,
    /// Posicion y ancho originales en Main.ini (BUTTONINFO).
    px: i32,
    py: i32,
    pw: i32,
}

#[derive(Clone, Copy)]
struct KeyRect {
    key: Key,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

// Geometria tomada de la tabla BUTTONINFO de Main.ini del fabricante:
// paso de teclas = 29 px, origen (298,140), 6 filas.  Se escala a unidades
// de 30 px para que el dibujo sea comodo en pantalla.
const PX_UNIT: f64 = 29.0;
const KEY_W: f64 = 30.0;
const KEY_H: f64 = 30.0;
const GAP: f64 = 3.0;

/// Disposición completa (100%, 104 teclas) del Redragon K535-KB,
/// con las posiciones reales del archivo Main.ini y etiquetas US QWERTY.
fn keyboard_layout() -> Vec<Vec<Option<Key>>> {
    fn k(label: &'static str, index: Option<u16>, px: i32, py: i32, w: i32) -> Option<Key> {
        Some(Key {
            index,
            label,
            px,
            py,
            pw: w,
        })
    }
    let v = |label: &'static str, index: Option<u16>, px: i32, py: i32, w: i32| {
        k(label, index, px, py, w)
    };
    // Las coordenadas (x,y,ancho) son las de BUTTONINFO en Main.ini.
    // none() = hueco entre bloques.
    let gap = || None;
    vec![
        // F0: Esc, F1-F12, PrintScreen, ScrollLock, Pause
        vec![
            v("Esc", Some(0), 298, 140, 19),
            gap(),
            v("F1", Some(14), 355, 140, 19),
            v("F2", Some(15), 384, 140, 18),
            v("F3", Some(16), 413, 140, 18),
            v("F4", Some(35), 442, 140, 18),
            gap(),
            v("F5", Some(36), 485, 140, 19),
            v("F6", Some(37), 513, 140, 19),
            v("F7", Some(56), 542, 140, 18),
            v("F8", Some(57), 571, 140, 18),
            gap(),
            v("F9", Some(58), 614, 140, 18),
            v("F10", Some(119), 643, 140, 18),
            v("F11", Some(120), 671, 140, 19),
            v("F12", Some(121), 700, 140, 18),
            gap(),
            v("PrtSc", Some(130), 736, 140, 18),
            v("ScrLk", Some(131), 765, 140, 18),
            v("Pause", Some(132), 794, 140, 18),
        ],
        // F1: ` 1-0 - = BackSpace | Ins Home PgUp | NumLock / * -
        vec![
            v("`~", Some(21), 298, 174, 19),
            v("1", Some(22), 327, 174, 18),
            v("2", Some(23), 355, 174, 19),
            v("3", Some(24), 384, 174, 18),
            v("4", Some(25), 413, 174, 18),
            v("5", Some(26), 442, 174, 18),
            v("6", Some(27), 470, 174, 19),
            v("7", Some(28), 499, 174, 19),
            v("8", Some(29), 528, 174, 18),
            v("9", Some(30), 557, 174, 18),
            v("0", Some(31), 585, 174, 18),
            v("-_", Some(32), 614, 174, 18),
            v("=+", Some(33), 643, 174, 18),
            v("Bksp", Some(34), 671, 174, 47),
            gap(),
            v("Ins", Some(41), 736, 174, 18),
            v("Home", Some(96), 765, 174, 18),
            v("PgUp", Some(97), 794, 174, 18),
            gap(),
            v("NumLk", Some(99), 830, 174, 18),
            v("/", None, 859, 174, 18),
            v("*", None, 887, 174, 19),
            v("-", None, 916, 174, 18),
        ],
        // F2: Tab Q..P [ ] \ | Del End PgDn | Num7 8 9 +
        vec![
            v("Tab", Some(42), 298, 202, 33),
            v("Q", Some(43), 342, 202, 18),
            v("W", Some(44), 370, 202, 18),
            v("E", Some(45), 399, 202, 18),
            v("R", Some(46), 427, 202, 19),
            v("T", Some(47), 456, 202, 18),
            v("Y", Some(48), 485, 202, 19),
            v("U", Some(49), 513, 202, 19),
            v("I", Some(50), 542, 202, 18),
            v("O", Some(51), 571, 202, 18),
            v("P", Some(52), 599, 202, 19),
            v("[{", Some(53), 628, 202, 18),
            v("]}", Some(54), 657, 202, 18),
            v("\\|", Some(55), 684, 202, 37),
            gap(),
            v("Del", Some(76), 736, 202, 18),
            v("End", Some(104), 765, 202, 18),
            v("PgDn", Some(125), 794, 202, 18),
            gap(),
            v("7", Some(98), 830, 202, 18),
            v("8", Some(60), 859, 202, 18),
            v("9", Some(61), 887, 202, 19),
            v("+", Some(62), 916, 202, 18),
        ],
        // F3: Caps A..L ; ' Enter | Num4 5 6
        vec![
            v("Caps", Some(63), 298, 231, 40),
            v("A", Some(64), 348, 231, 18),
            v("S", Some(65), 377, 231, 18),
            v("D", Some(66), 406, 231, 18),
            v("F", Some(67), 435, 231, 18),
            v("G", Some(68), 463, 231, 18),
            v("H", Some(69), 492, 231, 18),
            v("J", Some(70), 521, 231, 18),
            v("K", Some(71), 549, 231, 19),
            v("L", Some(72), 578, 231, 18),
            v(";:", Some(73), 607, 231, 18),
            v("'\"", Some(74), 635, 231, 19),
            v("Enter", Some(75), 679, 231, 39),
            gap(),
            v("4", Some(80), 830, 231, 18),
            v("5", Some(81), 859, 231, 18),
            v("6", Some(82), 887, 231, 19),
        ],
        // F4: Shift Z..M , . / RShift | Up | Num1 2 3 Enter
        vec![
            v("Shift", Some(84), 298, 260, 55),
            v("Z", Some(85), 363, 260, 18),
            v("X", Some(86), 391, 260, 19),
            v("C", Some(87), 420, 260, 18),
            v("V", Some(88), 449, 260, 18),
            v("B", Some(89), 478, 260, 18),
            v("N", Some(90), 506, 260, 18),
            v("M", Some(91), 535, 260, 18),
            v(",<", Some(92), 564, 260, 18),
            v(".>", Some(93), 592, 260, 18),
            v("/?", Some(94), 621, 260, 18),
            v("RShift", Some(95), 679, 260, 39),
            gap(),
            v("Up", Some(114), 765, 260, 18),
            gap(),
            v("1", Some(101), 830, 260, 18),
            v("2", Some(102), 859, 260, 18),
            v("3", Some(103), 887, 260, 19),
            v("Ent", Some(124), 916, 260, 18),
        ],
        // F5: Ctrl Win Alt Space RAlt Fn Menu RCtrl | Left Down Right | Num0 .
        vec![
            v("Ctrl", Some(105), 298, 289, 26),
            v("Win", Some(106), 334, 289, 26),
            v("Alt", Some(107), 370, 289, 25),
            v("Space", Some(108), 406, 289, 169),
            v("RAlt", Some(109), 585, 289, 26),
            v("Fn", Some(110), 621, 289, 26),
            v("Menu", Some(111), 657, 289, 26),
            v("RCtrl", Some(112), 693, 289, 25),
            gap(),
            v("Lt", Some(115), 736, 289, 18),
            v("Dn", Some(116), 765, 289, 18),
            v("Rt", Some(117), 794, 289, 18),
            gap(),
            v("0", Some(123), 830, 289, 47),
            v(".", Some(122), 887, 289, 19),
        ],
    ]
}

fn layout_rects() -> Vec<KeyRect> {
    // Main.ini dibuja los tres bloques demasiado juntos para una pantalla;
    // los separamos usando un offset fijo por bloque y una tecla de 1u de 28 px.
    keyboard_layout()
        .into_iter()
        .flatten()
        .flatten()
        .map(|key| {
            let (x, w) = if key.px >= 830 {
                // teclado numerico
                (590.0 + (key.px - 830) as f64 / PX_UNIT * KEY_W, key.pw)
            } else if key.px >= 736 {
                // cluster de navegacion
                (480.0 + (key.px - 736) as f64 / PX_UNIT * KEY_W, key.pw)
            } else {
                // bloque principal
                ((key.px - 298) as f64 / PX_UNIT * KEY_W, key.pw)
            };
            KeyRect {
                x,
                y: (key.py - 140) as f64 / PX_UNIT * KEY_H,
                w: w as f64 / 18.0 * 28.0,
                h: KEY_H - GAP,
                key,
            }
        })
        .collect()
}

fn key_name(index: u16) -> String {
    KEYMAP
        .iter()
        .find(|(i, _)| *i == index)
        .map(|(_, n)| (*n).to_string())
        .unwrap_or_else(|| format!("#{index}"))
}

fn rgba(rgb: [u8; 3]) -> gdk::RGBA {
    gdk::RGBA::new(
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
        1.0,
    )
}

fn rgb_of(c: &gdk::RGBA) -> [u8; 3] {
    [
        (c.red() * 255.0).round() as u8,
        (c.green() * 255.0).round() as u8,
        (c.blue() * 255.0).round() as u8,
    ]
}

fn install_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, 600);
    }
}

fn section(title: &str) -> gtk::Label {
    let lbl = gtk::Label::new(Some(title));
    lbl.set_halign(gtk::Align::Start);
    lbl.set_margin_top(16);
    lbl.set_margin_bottom(6);
    lbl.add_css_class("section-title");
    lbl
}

fn card() -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 12);
    b.add_css_class("card");
    b
}

/// Dibuja una tecla con la libreria de cairo.
fn draw_key(cr: &cairo::Context, rect: &KeyRect, color: [u8; 3], hovered: bool, pressed: bool) {
    let r = 6.0;
    cr.new_sub_path();
    cr.arc(rect.x + rect.w - r, rect.y + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
    cr.arc(rect.x + rect.w - r, rect.y + rect.h - r, r, 0.0, std::f64::consts::FRAC_PI_2);
    cr.arc(rect.x + r, rect.y + rect.h - r, r, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
    cr.arc(rect.x + r, rect.y + r, r, std::f64::consts::PI, 3.0 * std::f64::consts::FRAC_PI_2);
    cr.close_path();

    let f = if pressed { 0.65 } else { 1.0 };
    cr.set_source_rgb(
        color[0] as f64 / 255.0 * f,
        color[1] as f64 / 255.0 * f,
        color[2] as f64 / 255.0 * f,
    );
    cr.fill_preserve().ok();
    cr.set_line_width(if hovered { 2.0 } else { 1.0 });
    cr.set_source_rgba(1.0, 1.0, 1.0, if hovered { 0.75 } else { 0.25 });
    cr.stroke().ok();

    // texto centrado
    let sum = color[0] as u32 + color[1] as u32 + color[2] as u32;
    cr.select_font_face(
        "Sans",
        cairo::FontSlant::Normal,
        cairo::FontWeight::Bold,
    );
    cr.set_font_size(if rect.key.label.len() > 3 { 9.0 } else { 11.0 });
    cr.set_source_rgb(
        if sum > 380 { 0.06 } else { 1.0 },
        if sum > 380 { 0.06 } else { 1.0 },
        if sum > 380 { 0.06 } else { 1.0 },
    );
    let ext = cr.text_extents(rect.key.label);
    if let Ok(ext) = ext {
        let tx = rect.x + (rect.w - ext.width()) / 2.0 - ext.x_bearing();
        let ty = rect.y + (rect.h - ext.height()) / 2.0 - ext.y_bearing();
        cr.move_to(tx, ty);
    }
    let _ = cr.show_text(rect.key.label);
}

fn build_ui(app: &gtk::Application) {
    install_css();

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Redragon K535 · Mitra RGB")
        .default_width(1000)
        .default_height(880)
        .build();

    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let (evt_tx, evt_rx) = mpsc::channel::<Evt>();
    thread::spawn(move || worker_loop(cmd_rx, evt_tx));

    let shared = Shared {
        tx: cmd_tx,
        profile: Rc::new(Cell::new(0)),
        selected_color: Rc::new(RefCell::new([255, 255, 255])),
        connected: Rc::new(Cell::new(false)),
        keys_profile: Rc::new(Cell::new(-1)),
    };
    let loading = Rc::new(Cell::new(false));

    // ---- contenedor -------------------------------------------------
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_start(26);
    content.set_margin_end(26);
    content.set_margin_top(20);
    content.set_margin_bottom(24);
    scroll.set_child(Some(&content));
    outer.append(&scroll);

    // ---- cabecera ---------------------------------------------------
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let title = gtk::Label::new(Some("Redragon K535"));
    title.set_halign(gtk::Align::Start);
    title.add_css_class("big-title");
    titles.append(&title);
    let subtitle = gtk::Label::new(Some(
        "Teclado externo Mitra RGB · perfiles, efectos y color por tecla",
    ));
    subtitle.set_halign(gtk::Align::Start);
    subtitle.add_css_class("caption");
    titles.append(&subtitle);
    header.append(&titles);

    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    header.append(&spacer);

    let badge = gtk::Label::new(Some("conectando…"));
    badge.add_css_class("badge");
    badge.set_valign(gtk::Align::Center);
    header.append(&badge);
    content.append(&header);

    // ---- perfil -----------------------------------------------------
    content.append(&section("Perfil"));
    let profile_card = card();
    let profile_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let profile_label = gtk::Label::new(Some("Perfil"));
    profile_label.set_size_request(70, -1);
    profile_label.set_halign(gtk::Align::Start);
    profile_row.append(&profile_label);

    let mut radios: Vec<gtk::CheckButton> = Vec::new();
    for p in 0..NPROFILES {
        let rb = gtk::CheckButton::with_label(&format!("Perfil {}", p + 1));
        if p == 0 {
            rb.set_active(true);
        }
        if let Some(first) = radios.first() {
            rb.set_group(Some(first));
        }
        let shared = shared.clone();
        let loading = loading.clone();
        rb.connect_toggled(move |rb| {
            if !rb.is_active() || loading.get() {
                return;
            }
            shared.profile.set(p);
            let _ = shared.tx.send(Cmd::Activate(p));
            let _ = shared.tx.send(Cmd::ReadKeys(p));
        });
        profile_row.append(&rb);
        radios.push(rb);
    }
    profile_card.append(&profile_row);
    content.append(&profile_card);

    // ---- iluminacion ------------------------------------------------
    content.append(&section("Iluminación"));
    let light_card = card();

    let mode_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let mode_lbl = gtk::Label::new(Some("Efecto"));
    mode_lbl.set_size_request(70, -1);
    mode_lbl.set_halign(gtk::Align::Start);
    let store = gio::ListStore::new::<gtk::StringObject>();
    for n in MODE_NAMES.iter() {
        store.append(&gtk::StringObject::new(if n.is_empty() { "—" } else { n }));
    }
    let mode = gtk::DropDown::new(Some(store), None::<&gtk::Expression>);
    mode.set_hexpand(true);
    mode_row.append(&mode_lbl);
    mode_row.append(&mode);
    light_card.append(&mode_row);

    let bright_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let bright_lbl = gtk::Label::new(Some("Brillo"));
    bright_lbl.set_size_request(70, -1);
    bright_lbl.set_halign(gtk::Align::Start);
    let bright_adj = gtk::Adjustment::new(0.0, 0.0, 25.0, 1.0, 5.0, 0.0);
    let bright = gtk::Scale::builder()
        .orientation(gtk::Orientation::Horizontal)
        .adjustment(&bright_adj)
        .draw_value(false)
        .hexpand(true)
        .build();
    bright.set_size_request(180, 40);
    let bright_val = gtk::Label::new(Some("0"));
    bright_val.set_size_request(42, -1);
    bright_val.set_halign(gtk::Align::End);
    bright_row.append(&bright_lbl);
    bright_row.append(&bright);
    bright_row.append(&bright_val);
    light_card.append(&bright_row);

    let speed_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let speed_lbl = gtk::Label::new(Some("Velocidad"));
    speed_lbl.set_size_request(70, -1);
    speed_lbl.set_halign(gtk::Align::Start);
    let speed_adj = gtk::Adjustment::new(0.0, 0.0, 5.0, 1.0, 1.0, 0.0);
    let speed = gtk::Scale::builder()
        .orientation(gtk::Orientation::Horizontal)
        .adjustment(&speed_adj)
        .draw_value(false)
        .hexpand(true)
        .build();
    speed.set_size_request(180, 40);
    let speed_val = gtk::Label::new(Some("0"));
    speed_val.set_size_request(42, -1);
    speed_val.set_halign(gtk::Align::End);
    speed_row.append(&speed_lbl);
    speed_row.append(&speed);
    speed_row.append(&speed_val);
    light_card.append(&speed_row);

    let power_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let power_lbl = gtk::Label::new(Some("Encendido"));
    power_lbl.set_size_request(70, -1);
    power_lbl.set_halign(gtk::Align::Start);
    power_lbl.set_hexpand(true);
    let power = gtk::Switch::new();
    power.set_valign(gtk::Align::Center);
    power_row.append(&power_lbl);
    power_row.append(&power);
    light_card.append(&power_row);

    content.append(&light_card);

    // ---- color -------------------------------------------------------
    content.append(&section("Color"));
    let color_card = card();
    let color_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let color_lbl = gtk::Label::new(Some("Tono"));
    color_lbl.set_size_request(70, -1);
    color_lbl.set_halign(gtk::Align::Start);

    // muestra de color: boton con un area de dibujo dentro
    let swatch = gtk::Button::new();
    swatch.add_css_class("flat-button");
    swatch.set_size_request(72, 40);
    swatch.set_tooltip_text(Some("Elige el color"));
    let swatch_draw = gtk::DrawingArea::new();
    let swatch_color = Rc::new(Cell::new([255, 255, 255]));
    {
        let swatch_color = swatch_color.clone();
        swatch_draw.set_draw_func(move |_area, cr, w, h| {
            let c = swatch_color.get();
            cr.set_source_rgb(
                c[0] as f64 / 255.0,
                c[1] as f64 / 255.0,
                c[2] as f64 / 255.0,
            );
            cr.rectangle(0.0, 0.0, w as f64, h as f64);
            let _ = cr.fill();
        });
    }
    swatch.set_child(Some(&swatch_draw));

    let apply_profile = gtk::Button::with_label("Aplicar al perfil");
    apply_profile.set_valign(gtk::Align::Center);
    apply_profile.add_css_class("flat-button");

    let apply_all = gtk::Button::with_label("Aplicar a todas las teclas");
    apply_all.set_valign(gtk::Align::Center);
    apply_all.add_css_class("flat-button");

    color_row.append(&color_lbl);
    color_row.append(&swatch);
    color_row.append(&apply_profile);
    color_row.append(&apply_all);
    color_card.append(&color_row);
    content.append(&color_card);

    // ---- teclado -----------------------------------------------------
    content.append(&section("Color por tecla"));
    let kb_card = card();
    let rects = Rc::new(layout_rects());
    let total_w = rects
        .iter()
        .map(|r| r.x + r.w)
        .fold(0.0f64, f64::max)
        + GAP;
    let total_h = rects
        .iter()
        .map(|r| r.y + r.h)
        .fold(0.0f64, f64::max)
        + GAP;

    let key_colors: Rc<RefCell<HashMap<u16, [u8; 3]>>> = Rc::new(RefCell::new(HashMap::new()));
    let hovered: Rc<Cell<Option<u16>>> = Rc::new(Cell::new(None));
    let pressed: Rc<Cell<Option<u16>>> = Rc::new(Cell::new(None));

    let area = gtk::DrawingArea::new();
    area.set_content_width(total_w as i32);
    area.set_content_height(total_h as i32);
    area.set_size_request(total_w as i32, total_h as i32);
    area.set_halign(gtk::Align::Center);
    {
        let rects = rects.clone();
        let key_colors = key_colors.clone();
        let hovered = hovered.clone();
        let pressed = pressed.clone();
        area.set_draw_func(move |_area, cr, _w, _h| {
            cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
            for rect in rects.iter() {
                let color = rect
                    .key
                    .index
                    .and_then(|i| key_colors.borrow().get(&i).copied())
                    .unwrap_or(if rect.key.index.is_some() {
                        [38, 38, 44]
                    } else {
                        [26, 26, 30]
                    });
                let hv = rect.key.index.is_some() && hovered.get() == rect.key.index;
                let pr = rect.key.index.is_some() && pressed.get() == rect.key.index;
                draw_key(cr, rect, color, hv, pr);
            }
        });
    }

    // clic en una tecla -> selector de color para esa tecla
    {
        let rects = rects.clone();
        let pressed = pressed.clone();
        let pressed2 = pressed.clone();
        let key_colors = key_colors.clone();
        let shared = shared.clone();
        let win = window.clone();
        let area_click = area.clone();
        let click = gtk::GestureClick::new();
        click.connect_pressed(move |_g, _n, x, y| {
            let hit = rects.iter().find(|r| {
                x >= r.x && x <= r.x + r.w && y >= r.y && y <= r.y + r.h
            });
            if let Some(rect) = hit {
                let Some(index) = rect.key.index else {
                    area_click.set_tooltip_text(Some(&format!(
                        "{} · sin indice asignado (tecla del bloque extendido)",
                        rect.key.label
                    )));
                    return;
                };
                pressed.set(Some(index));
                area_click.queue_draw();

                let current = key_colors
                    .borrow()
                    .get(&index)
                    .copied()
                    .unwrap_or([255, 255, 255]);
                let dialog = gtk::ColorDialog::new();
                let shared2 = shared.clone();
                dialog.choose_rgba(
                    Some(&win),
                    Some(&rgba(current)),
                    None::<&gio::Cancellable>,
                    move |res| {
                        if let Ok(c) = res {
                            let rgb = rgb_of(&c);
                            let p = shared2.profile.get();
                            let _ = shared2.tx.send(Cmd::KeyColor {
                                profile: p,
                                index,
                                rgb,
                            });
                        }
                    },
                );
            }
        });
        let pressed2 = pressed2.clone();
        click.connect_released(move |_g, _n, _x, _y| {
            pressed2.set(None);
        });
        area.add_controller(click);
    }

    // hover + tooltip con el nombre real de la tecla
    {
        let rects = rects.clone();
        let hovered = hovered.clone();
        let hovered2 = hovered.clone();
        let area_motion = area.clone();
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(move |_c, x, y| {
            let hit = rects.iter().find(|r| {
                x >= r.x && x <= r.x + r.w && y >= r.y && y <= r.y + r.h
            });
            let new = hit.and_then(|r| r.key.index);
            if new != hovered.get() {
                hovered.set(new);
                area_motion.queue_draw();
            }
            if let Some(rect) = hit {
                let tip = match rect.key.index {
                    Some(i) => {
                        let est = if i >= 38 && (i <= 41 || (i >= 59 && i <= 62) ||
                                   (i >= 76 && i <= 117) || i >= 122) {
                            " · índice estimado"
                        } else {
                            ""
                        };
                        format!("{} · indice {}{}", key_name(i), i, est)
                    }
                    None => format!("{} · sin indice asignado", rect.key.label),
                };
                area_motion.set_tooltip_text(Some(&tip));
            }
        });
        let hovered2 = hovered2.clone();
        motion.connect_leave(move |_c| {
            hovered2.set(None);
        });
        area.add_controller(motion);
    }

    let kb_outer = gtk::Box::new(gtk::Orientation::Vertical, 6);
    kb_outer.set_halign(gtk::Align::Center);
    kb_outer.append(&area);
    kb_card.append(&kb_outer);
    let kb_hint = gtk::Label::new(Some(
        "Teclado 100% (US QWERTY) · clic para colorear · índices del bloque extendido estimados (dime si alguna tecla no corresponde)",
    ));
    kb_hint.set_halign(gtk::Align::Start);
    kb_hint.set_wrap(true);
    kb_hint.add_css_class("caption");
    kb_card.append(&kb_hint);
    content.append(&kb_card);

    // ---- barra de estado ---------------------------------------------
    let status = gtk::Label::new(Some("Iniciando…"));
    status.set_halign(gtk::Align::Start);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    status.add_css_class("statusbar");
    outer.append(&status);

    window.set_child(Some(&outer));

    // ---- selector de color ---------------------------------------------
    {
        let shared = shared.clone();
        let win = window.clone();
        let swatch_color = swatch_color.clone();
        swatch.connect_clicked(move |_| {
            let current = *shared.selected_color.borrow();
            let dialog = gtk::ColorDialog::new();
            let shared2 = shared.clone();
            let swatch_color2 = swatch_color.clone();
            let swatch_draw2 = swatch_draw.clone();
            dialog.choose_rgba(
                Some(&win),
                Some(&rgba(current)),
                None::<&gio::Cancellable>,
                move |res| {
                    if let Ok(c) = res {
                        let rgb = rgb_of(&c);
                        *shared2.selected_color.borrow_mut() = rgb;
                        swatch_color2.set(rgb);
                        swatch_draw2.queue_draw();
                    }
                },
            );
        });
    }

    // ---- aplicar el color -----------------------------------------------
    {
        let shared = shared.clone();
        apply_profile.connect_clicked(move |_| {
            let p = shared.profile.get();
            let rgb = *shared.selected_color.borrow();
            let _ = shared.tx.send(Cmd::ProfileColor { profile: p, rgb });
        });
    }

    // ---- aplicar a todas las teclas -------------------------------------
    {
        let shared = shared.clone();
        apply_all.connect_clicked(move |_| {
            let p = shared.profile.get();
            let rgb = *shared.selected_color.borrow();
            let _ = shared.tx.send(Cmd::AllKeys { profile: p, rgb });
        });
    }

    // ---- sliders con rebote ------------------------------------------------
    // NO cancelamos timeouts: timeout_add_local_once se autodestruye al
    // dispararse y SourceId::remove() hace unwrap() -> panic dentro de GTK.
    // En su lugar usamos un contador de generacion: cada movimiento invalidar
    // los temporizadores anteriores sin tocar sus SourceId.
    let bright_gen: Rc<Cell<u64>> = Rc::new(Cell::new(0));
    let speed_gen: Rc<Cell<u64>> = Rc::new(Cell::new(0));

    {
        let shared = shared.clone();
        let loading = loading.clone();
        let bright_val = bright_val.clone();
        let gen = bright_gen.clone();
        bright.connect_value_changed(move |scale| {
            if loading.get() {
                return;
            }
            let v = scale.value().round() as u8;
            bright_val.set_text(&v.to_string());
            gen.set(gen.get() + 1);
            let mine = gen.get();
            let shared2 = shared.clone();
            let gen2 = gen.clone();
            glib::timeout_add_local_once(Duration::from_millis(220), move || {
                if gen2.get() != mine {
                    return;
                }
                let p = shared2.profile.get();
                let _ = shared2.tx.send(Cmd::Field {
                    profile: p,
                    offset: 0x01,
                    value: v,
                    label: "brillo",
                });
            });
        });
    }

    {
        let shared = shared.clone();
        let loading = loading.clone();
        let speed_val = speed_val.clone();
        let gen = speed_gen.clone();
        speed.connect_value_changed(move |scale| {
            if loading.get() {
                return;
            }
            let v = scale.value().round() as u8;
            speed_val.set_text(&v.to_string());
            gen.set(gen.get() + 1);
            let mine = gen.get();
            let shared2 = shared.clone();
            let gen2 = gen.clone();
            glib::timeout_add_local_once(Duration::from_millis(220), move || {
                if gen2.get() != mine {
                    return;
                }
                let p = shared2.profile.get();
                let _ = shared2.tx.send(Cmd::Field {
                    profile: p,
                    offset: 0x02,
                    value: v,
                    label: "velocidad",
                });
            });
        });
    }

    // ---- modo ---------------------------------------------------------------
    {
        let shared = shared.clone();
        let loading = loading.clone();
        let mode_ref = mode.clone();
        mode.connect_selected_notify(move |_| {
            if loading.get() {
                return;
            }
            let idx = mode_ref.selected() as u8;
            if idx == 0 {
                return;
            }
            let p = shared.profile.get();
            let _ = shared.tx.send(Cmd::Field {
                profile: p,
                offset: 0x00,
                value: idx,
                label: "modo",
            });
        });
    }

    // ---- encendido ------------------------------------------------------------
    {
        let shared = shared.clone();
        let loading = loading.clone();
        power.connect_active_notify(move |sw| {
            if loading.get() {
                return;
            }
            let p = shared.profile.get();
            let _ = shared.tx.send(Cmd::Field {
                profile: p,
                offset: 0x04,
                value: if sw.is_active() { 1 } else { 0 },
                label: "encendido",
            });
        });
    }

    // ---- recepcion de eventos ----------------------------------------------------
    {
        let shared = shared.clone();
        let loading = loading.clone();
        let mode = mode.clone();
        let bright = bright.clone();
        let speed = speed.clone();
        let power = power.clone();
        let badge = badge.clone();
        let status = status.clone();
        let bright_val = bright_val.clone();
        let speed_val = speed_val.clone();
        let key_colors = key_colors.clone();
        let area = area.clone();
        let radios = radios.clone();

        glib::timeout_add_local(Duration::from_millis(REFRESH_MS), move || {
            while let Ok(evt) = evt_rx.try_recv() {
                match evt {
                    Evt::State(st) => {
                        shared.connected.set(st.connected);
                        // Si el dispositivo esta en otro perfil, recargar su mapa
                        if st.active >= 0 && shared.keys_profile.get() != st.active {
                            shared.keys_profile.set(st.active);
                            let _ = shared.tx.send(Cmd::ReadKeys(st.active as u8));
                        }
                        let p = shared.profile.get() as usize;
                        let idx = p.min(NPROFILES as usize - 1);
                        let prof = st.profiles[idx];

                        loading.set(true);
                        mode.set_selected(prof.mode as u32);
                        bright.set_value(prof.brightness as f64);
                        speed.set_value(prof.speed as f64);
                        power.set_active(prof.on != 0);
                        if st.active >= 0 && (st.active as usize) < radios.len() {
                            radios[st.active as usize].set_active(true);
                        }
                        loading.set(false);

                        bright_val.set_text(&prof.brightness.to_string());
                        speed_val.set_text(&prof.speed.to_string());

                        badge.set_text(if st.connected {
                            "conectado"
                        } else {
                            "sin conexión"
                        });
                        badge.remove_css_class("badge-ko");
                        badge.remove_css_class("badge-ok");
                        badge.add_css_class(if st.connected { "badge-ok" } else { "badge-ko" });

                        // colores por tecla escritos en esta sesion
                        let session = st.key_colors.clone();
                        let mut map = key_colors.borrow_mut();
                        for ((prof_id, index), rgb) in session {
                            if prof_id == shared.profile.get() {
                                map.insert(index, rgb);
                            }
                        }
                        drop(map);
                        area.queue_draw();
                    }
                    Evt::Keys(map) => {
                        let mut cur = key_colors.borrow_mut();
                        *cur = *map;
                        drop(cur);
                        area.queue_draw();
                    }
                    Evt::Error(msg) => {
                        shared.connected.set(false);
                        badge.set_text("sin conexión");
                        badge.remove_css_class("badge-ok");
                        badge.add_css_class("badge-ko");
                        status.set_text(&msg);
                    }
                    Evt::Note(msg) => {
                        status.set_text(&msg);
                    }
                }
            }
            glib::ControlFlow::Continue
        });
    }

    // reconexion automatica si el teclado se desconecta
    {
        let shared = shared.clone();
        glib::timeout_add_local(Duration::from_millis(RECONNECT_MS), move || {
            if !shared.connected.get() {
                let _ = shared.tx.send(Cmd::Refresh);
            }
            glib::ControlFlow::Continue
        });
    }

    // arranque
    {
        let shared = shared.clone();
        let _ = shared.tx.send(Cmd::Refresh);
        let _ = shared.tx.send(Cmd::ReadKeys(0));
    }

    window.present();
}

fn main() {
    let app = gtk::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run();
}
