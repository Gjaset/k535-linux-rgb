//! Protocolo HID del teclado Redragon K535-KB ("Mitra").
//!
//! Implementacion Rust de lo que hace la aplicacion de Windows del
//! fabricante, documentado en `../RE-NOTES.md`:
//!
//! * informes de 64 bytes, `Report ID 4` (coleccion de fabricante 0xFF1C/0x92)
//! * checksum u16 little-endian de los bytes 3..63 en los bytes 1..2
//! * payload en los bytes 8..63 (56 bytes maximo)
//! * perfiles de 42 bytes en `perfil * 0x2a`
//! * colores por tecla en `perfil * 512 + indice * 3`

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

pub const REPORT_LEN: usize = 64;
pub const MAX_PAYLOAD: usize = 56;

pub const VENDOR_ID: u16 = 0x0c45;
pub const PRODUCT_ID: u16 = 0x5004;

/// Coleccion de fabricante: usage page 0xFF1C, usage 0x92.
const VENDOR_SIGNATURE: [u8; 5] = [0x06, 0x1c, 0xff, 0x09, 0x92];

// ---------------------------------------------------------------- comandos
pub const CMD_BEGIN: u8 = 0x01;
pub const CMD_COMMIT: u8 = 0x02;
pub const CMD_READ_CFG: u8 = 0x03;
pub const CMD_WRITE_CFG: u8 = 0x04;
pub const CMD_READ_MEM: u8 = 0x05;
pub const CMD_WRITE_MEM: u8 = 0x06;
pub const CMD_SET_KEY: u8 = 0x11;

// ------------------------------------------------------------ bloque perfil
pub const PROF_STRIDE: u16 = 0x2a;
pub const P_MODE: u8 = 0x00;
pub const P_BRIGHT: u8 = 0x01;
pub const P_SPEED: u8 = 0x02;
pub const P_ON: u8 = 0x04;
pub const P_COLOR: u8 = 0x05;
pub const P_PROFILE_IDX: u8 = 0x0a;
pub const P_DIRECTION: u8 = 0x0f;
pub const NPROFILES: u8 = 3;
pub const KEYMAP_STRIDE: u16 = 512;
/// Base absoluta del mapa de colores en la memoria del dispositivo.
/// El comando 0x11 (SET_KEY) interpreta su argumento de direccion como un
/// offset relativo a esta base; el comando 0x05 (READ_MEM) necesita la
/// direccion absoluta.
pub const KEYMAP_BASE: u16 = 0x0580;
/// Numero de teclas del dispositivo (indices 0..132 en los ficheros .userd).
pub const KEY_COUNT: u16 = 133;

/// Nombres de los efectos (apartado `[Mode]` de `Main.ini`).
pub const MODE_NAMES: [&str; 21] = [
    "",
    "Go with the stream",
    "Clouds fly",
    "Winding paths",
    "The trial of light",
    "Breathing",
    "Normally on",
    "Pass without trace",
    "Ripple graff",
    "Fast run without trace",
    "Snow winter jasmine",
    "Flowers blooming",
    "Swift action",
    "Hurricane",
    "Accumulate",
    "Digital Times",
    "Both ways",
    "Surmount",
    "Fast and the Furious",
    "",
    "Coastal",
];

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    NotFound,
    Timeout,
    Rejected(u8),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "error de E/S: {e}"),
            Error::NotFound => write!(
                f,
                "no se encontro el teclado Redragon K535-KB (0c45:5004)"
            ),
            Error::Timeout => write!(f, "el teclado no respondio"),
            Error::Rejected(c) => write!(f, "el teclado rechazo el comando 0x{c:02x}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error::Io(io::Error::new(io::ErrorKind::InvalidInput, s.to_string()))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Estado de un perfil, tal y como vive en el dispositivo.
#[derive(Clone, Copy, Debug)]
pub struct Profile {
    pub mode: u8,
    pub brightness: u8,
    pub speed: u8,
    pub on: u8,
    pub color: [u8; 3],
    pub profile_id: u8,
    pub direction: u8,
    pub raw: [u8; 42],
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            mode: 0,
            brightness: 0,
            speed: 0,
            on: 0,
            color: [0, 0, 0],
            profile_id: 0,
            direction: 0,
            raw: [0u8; 42],
        }
    }
}

impl Profile {
    fn parse(raw: [u8; 42]) -> Self {
        Profile {
            mode: raw[P_MODE as usize],
            brightness: raw[P_BRIGHT as usize],
            speed: raw[P_SPEED as usize],
            on: raw[P_ON as usize],
            color: [
                raw[P_COLOR as usize],
                raw[P_COLOR as usize + 1],
                raw[P_COLOR as usize + 2],
            ],
            profile_id: raw[P_PROFILE_IDX as usize],
            direction: raw[P_DIRECTION as usize],
            raw,
        }
    }

    pub fn mode_name(&self) -> &'static str {
        MODE_NAMES
            .get(self.mode as usize)
            .copied()
            .filter(|s| !s.is_empty())
            .unwrap_or("—")
    }
}

/// Estado completo del teclado.
#[derive(Clone, Debug, Default)]
pub struct State {
    pub profiles: [Profile; 3],
    pub active: i32,
    pub connected: bool,
    /// Colores por tecla cacheados (lo que se ha escrito desde esta sesion).
    pub key_colors: std::collections::HashMap<(u8, u16), [u8; 3]>,
}

/// Dispositivo HID abierto.
pub struct Device {
    file: File,
    pub path: PathBuf,
}

impl Device {
    /// Abre el nodo hidraw de la interfaz de fabricante, buscandolo solo.
    pub fn open() -> Result<Self> {
        let path = find_device().ok_or(Error::NotFound)?;
        Self::open_path(&path)
    }

    pub fn open_path(path: &Path) -> Result<Self> {
        let file = File::options().read(true).write(true).open(path)?;
        Ok(Device {
            file,
            path: path.to_path_buf(),
        })
    }

    // ------------------------------------------------------------- nucleo

    fn exchange(&mut self, req: &[u8; REPORT_LEN]) -> Result<[u8; REPORT_LEN]> {
        self.file.write_all(req)?;

        let mut pfd = libc::pollfd {
            fd: self.file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, 1000) };
        if rc < 0 {
            return Err(Error::Io(io::Error::last_os_error()));
        }
        if rc == 0 {
            return Err(Error::Timeout);
        }

        let mut buf = [0u8; REPORT_LEN];
        let mut got = 0;
        while got < REPORT_LEN {
            match self.file.read(&mut buf[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Error::Io(e)),
            }
        }
        Ok(buf)
    }

    /// Envia un comando y devuelve la respuesta completa.
    pub fn command(
        &mut self,
        cmd: u8,
        args: [u8; 4],
        payload: &[u8],
    ) -> Result<[u8; REPORT_LEN]> {
        if payload.len() > MAX_PAYLOAD {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "payload demasiado grande",
            )));
        }
        let mut req = [0u8; REPORT_LEN];
        req[0] = 0x04;
        req[3] = cmd;
        req[4..8].copy_from_slice(&args);
        req[8..8 + payload.len()].copy_from_slice(payload);

        let sum: u16 = req[3..].iter().fold(0u16, |a, &b| a.wrapping_add(b as u16));
        req[1] = (sum & 0xff) as u8;
        req[2] = (sum >> 8) as u8;

        let resp = self.exchange(&req)?;
        if resp[7] == 0xff {
            return Err(Error::Rejected(cmd));
        }
        Ok(resp)
    }

    // ---------------------------------------------------------- memoria

    pub fn read_mem(&mut self, addr: u16, len: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let chunk = (len - out.len()).min(MAX_PAYLOAD);
            let a = addr.wrapping_add(out.len() as u16);
            let args = [chunk as u8, (a & 0xff) as u8, (a >> 8) as u8, 0];
            let resp = self.command(CMD_READ_MEM, args, &[])?;
            out.extend_from_slice(&resp[8..8 + chunk]);
        }
        Ok(out)
    }

    pub fn write_mem(&mut self, addr: u16, data: &[u8]) -> Result<()> {
        let mut done = 0;
        while done < data.len() {
            let chunk = (data.len() - done).min(MAX_PAYLOAD);
            let a = addr.wrapping_add(done as u16);
            let args = [chunk as u8, (a & 0xff) as u8, (a >> 8) as u8, 0];
            self.command(CMD_WRITE_MEM, args, &data[done..done + chunk])?;
            done += chunk;
        }
        Ok(())
    }

    pub fn read_cfg(&mut self) -> Result<[u8; 44]> {
        let resp = self.command(CMD_READ_CFG, [44, 0, 0, 0], &[])?;
        let mut cfg = [0u8; 44];
        cfg.copy_from_slice(&resp[8..8 + 44]);
        Ok(cfg)
    }

    pub fn write_cfg(&mut self, cfg: &[u8; 44]) -> Result<()> {
        self.command(CMD_WRITE_CFG, [44, 0, 0, 0], cfg)?;
        Ok(())
    }

    // ------------------------------------------------------- operaciones

    pub fn begin(&mut self) -> Result<()> {
        self.command(CMD_BEGIN, [0; 4], &[])?;
        Ok(())
    }

    pub fn commit(&mut self) -> Result<()> {
        self.command(CMD_COMMIT, [0; 4], &[])?;
        Ok(())
    }

    /// Escribe un byte del bloque de un perfil (transaccion begin/commit).
    pub fn set_profile_field(&mut self, profile: u8, offset: u8, value: u8) -> Result<()> {
        self.begin()?;
        let addr = profile as u16 * PROF_STRIDE + offset as u16;
        self.write_mem(addr, &[value])?;
        self.commit()
    }

    /// Escribe 3 bytes RGB del bloque de un perfil.
    pub fn set_profile_color(&mut self, profile: u8, rgb: [u8; 3]) -> Result<()> {
        self.begin()?;
        let addr = profile as u16 * PROF_STRIDE + P_COLOR as u16;
        self.write_mem(addr, &rgb)?;
        self.commit()
    }

    /// Escribe el color de una tecla dentro de una transaccion ya abierta.
    pub fn set_key_color_raw(&mut self, profile: u8, index: u16, rgb: [u8; 3]) -> Result<()> {
        let addr = profile as u16 * KEYMAP_STRIDE + index * 3;
        self.command(
            CMD_SET_KEY,
            [3, (addr & 0xff) as u8, (addr >> 8) as u8, 0],
            &rgb,
        )?;
        Ok(())
    }

    /// Color de una tecla concreta (transaccion begin/commit).
    pub fn set_key_color(&mut self, profile: u8, index: u16, rgb: [u8; 3]) -> Result<()> {
        self.begin()?;
        self.set_key_color_raw(profile, index, rgb)?;
        self.commit()
    }

    /// Pinta varias teclas con una sola transaccion (mucho mas rapido).
    pub fn set_key_colors_bulk(&mut self, profile: u8, items: &[(u16, [u8; 3])]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        self.begin()?;
        for (index, rgb) in items {
            self.set_key_color_raw(profile, *index, *rgb)?;
        }
        self.commit()
    }

    /// Lee el mapa de colores por tecla de un perfil (512 bytes = 133 teclas).
    pub fn read_key_colors(&mut self, profile: u8) -> Result<std::collections::HashMap<u16, [u8; 3]>> {
        let buf = self.read_mem(
            KEYMAP_BASE.wrapping_add(profile as u16 * KEYMAP_STRIDE),
            KEYMAP_STRIDE as usize,
        )?;
        let mut map = std::collections::HashMap::new();
        for idx in 0..KEY_COUNT {
            let o = idx as usize * 3;
            if o + 3 <= buf.len() {
                map.insert(idx, [buf[o], buf[o + 1], buf[o + 2]]);
            }
        }
        Ok(map)
    }

    /// Cambia el perfil activo (byte +0x0a de la configuracion activa).
    pub fn activate_profile(&mut self, profile: u8) -> Result<()> {
        let mut cfg = self.read_cfg()?;
        cfg[P_PROFILE_IDX as usize] = profile;
        self.write_cfg(&cfg)?;
        let check = self.read_cfg()?;
        if check[P_PROFILE_IDX as usize] != profile {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::Other,
                "el teclado no cambio de perfil",
            )));
        }
        Ok(())
    }

    /// Lee el estado completo del teclado.
    pub fn read_state(&mut self) -> Result<State> {
        let mut profiles = [Profile::default(); 3];
        for (p, prof) in profiles.iter_mut().enumerate() {
            let raw = self.read_mem(p as u16 * PROF_STRIDE, 42)?;
            let mut buf = [0u8; 42];
            buf.copy_from_slice(&raw);
            *prof = Profile::parse(buf);
        }
        let active = match self.read_cfg() {
            Ok(cfg) => cfg[P_PROFILE_IDX as usize] as i32,
            Err(_) => -1,
        };
        Ok(State {
            profiles,
            active,
            connected: true,
            ..Default::default()
        })
    }
}

/// Busca el nodo hidraw de la interfaz de fabricante del K535.
pub fn find_device() -> Option<PathBuf> {
    let base = Path::new("/sys/class/hidraw");
    let mut found: Option<PathBuf> = None;
    for entry in fs::read_dir(base).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("hidraw") {
            continue;
        }
        let dev_dir = entry.path().join("device");
        let uevent = match fs::read_to_string(dev_dir.join("uevent")) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if !uevent.contains("HID_ID=0003:00000C45:00005004") {
            continue;
        }
        let desc = match fs::read(dev_dir.join("report_descriptor")) {
            Ok(d) => d,
            Err(_) => continue,
        };
        if !desc
            .windows(VENDOR_SIGNATURE.len())
            .any(|w| w == VENDOR_SIGNATURE)
        {
            continue;
        }
        let dev = PathBuf::from(format!("/dev/{name}"));
        match &found {
            Some(prev) if *prev <= dev => {}
            _ => found = Some(dev),
        }
    }
    found
}

/// Tabla indice de tecla -> nombre (indices verificados; ver RE-NOTES.md).
pub const KEYMAP: &[(u16, &str)] = &[
    (0, "Esc"),
    (21, "`/~"),
    (14, "F1"),
    (15, "F2"),
    (16, "F3"),
    (22, "1"),
    (23, "2"),
    (24, "3"),
    (25, "4"),
    (26, "5"),
    (27, "6"),
    (28, "7"),
    (29, "8"),
    (30, "9"),
    (31, "0"),
    (32, "-"),
    (33, "+"),
    (34, "BackSpace"),
    (35, "F4"),
    (36, "F5"),
    (37, "F6"),
    (42, "Tab"),
    (43, "Q"),
    (44, "W"),
    (45, "E"),
    (46, "R"),
    (47, "T"),
    (48, "Y"),
    (49, "U"),
    (50, "I"),
    (51, "O"),
    (52, "P"),
    (53, "["),
    (54, "]"),
    (55, "\\"),
    (56, "F7"),
    (57, "F8"),
    (58, "F9"),
    (63, "CapsLock"),
    (64, "A"),
    (65, "S"),
    (66, "D"),
    (67, "F"),
    (68, "G"),
    (69, "H"),
    (70, "J"),
    (71, "K"),
    (72, "L"),
    (73, ";"),
    (74, "'"),
    (75, "Enter"),
    (84, "Shift"),
    (85, "Z"),
    (86, "X"),
    (87, "C"),
    (88, "V"),
    (89, "B"),
    (90, "N"),
    (91, "M"),
    (92, ","),
    (93, "."),
    (94, "/"),
    (95, "RightShift"),
    (102, "K45"),
    (105, "Ctrl"),
    (106, "Win"),
    (107, "Alt"),
    (108, "Space"),
    (109, "RightAlt"),
    (110, "Fn"),
    (111, "App"),
    (112, "RightCtrl"),
    (119, "F10"),
    (120, "F11"),
    (121, "F12"),
];

/// Devuelve el indice de una tecla por nombre (sin distinguir mayusculas).
pub fn key_by_name(name: &str) -> Option<u16> {
    let lower = name.to_ascii_lowercase();
    KEYMAP
        .iter()
        .find(|(_, n)| n.to_ascii_lowercase() == lower)
        .map(|(i, _)| *i)
}
