//! k535ctl - CLI para el teclado Redragon K535-KB.
//!
//! Sustituye a la version en C: mismo protocolo, misma interfaz de linea de
//! comandos.

use k535::{Device, Error, Result, KEYMAP, MODE_NAMES, NPROFILES};

fn usage() {
    println!(
        "uso: k535ctl <orden> [args]\n\n\
         \x20 list                      interfaces hidraw y cual es la del teclado\n\
         \x20 keys                      indice de tecla -> nombre\n\
         \x20 dump                      perfiles y configuracion activa\n\
         \x20 profile <0-2>             activa un perfil\n\
         \x20 mode <perfil> <1-20>      modo de iluminacion\n\
         \x20 brightness <perfil> <0-25> brillo\n\
         \x20 speed <perfil> <0-5>      velocidad de animacion\n\
         \x20 color <perfil> <r> <g> <b> color RGB personalizado\n\
         \x20 on <perfil> <0|1>         encender/apagar\n\
         \x20 key <perfil> <tecla|indice> <r> <g> <b>   color por tecla\n\
         \x20 raw <hex...>              informe crudo (depuracion)"
    );
}

fn num(s: &str, max: u32) -> Option<u32> {
    let v = s.trim().parse::<u32>().ok()?;
    if v > max {
        None
    } else {
        Some(v)
    }
}

fn hexdump(tag: &str, data: &[u8]) {
    for (i, chunk) in data.chunks(16).enumerate() {
        if i == 0 {
            print!("{tag:<18}");
        } else {
            print!("{:<18}", "");
        }
        for b in chunk {
            print!(" {b:02x}");
        }
        println!();
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
        std::process::exit(2);
    }
    let cmd = args[0].as_str();

    if cmd == "keys" {
        println!("indice  nombre");
        for (i, n) in KEYMAP {
            println!("{i:5}   {n}");
        }
        return;
    }

    if cmd == "list" {
        match k535::find_device() {
            Some(p) => println!("interfaz de fabricante: {}", p.display()),
            None => println!("no se encontro el teclado Redragon K535-KB"),
        }
        return;
    }

    let mut dev = match Device::open() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let r = run(&mut dev, &args);
    if let Err(e) = r {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn run(dev: &mut Device, args: &[String]) -> Result<()> {
    let cmd = args[0].as_str();
    match cmd {
        "dump" => {
            let st = dev.read_state()?;
            for (p, prof) in st.profiles.iter().enumerate() {
                println!("\n=== perfil {p} (direccion 0x{:02x}) ===", p * 0x2a);
                hexdump("bloque:", &prof.raw);
                println!("  modo        : {} - {}", prof.mode, prof.mode_name());
                println!("  brillo      : {}", prof.brightness);
                println!("  velocidad   : {}", prof.speed);
                println!("  encendido   : {}", prof.on);
                println!(
                    "  color RGB   : {},{},{}",
                    prof.color[0], prof.color[1], prof.color[2]
                );
                println!("  indice prof.: {}", prof.profile_id);
                println!("  direccion   : {}", prof.direction);
            }
            println!("\nperfil activo: {}", st.active);
        }
        "profile" => {
            let p = num(&args[1], (NPROFILES - 1) as u32).ok_or("perfil invalido")?;
            dev.activate_profile(p as u8)?;
            println!("perfil activo: {p}");
        }
        "mode" | "brightness" | "speed" | "on" => {
            let p = num(&args[1], (NPROFILES - 1) as u32).ok_or("perfil invalido")? as u8;
            let (off, max) = match cmd {
                "mode" => (0x00u8, 20u32),
                "brightness" => (0x01, 25),
                "speed" => (0x02, 5),
                _ => (0x04, 1),
            };
            let v = num(&args[2], max).ok_or("valor fuera de rango")? as u8;
            dev.set_profile_field(p, off, v)?;
            dev.activate_profile(p)?;
            println!("perfil {p}: {cmd} = {v}");
        }
        "color" => {
            let p = num(&args[1], (NPROFILES - 1) as u32).ok_or("perfil invalido")? as u8;
            let r = num(&args[2], 255).ok_or("r invalido")? as u8;
            let g = num(&args[3], 255).ok_or("g invalido")? as u8;
            let b = num(&args[4], 255).ok_or("b invalido")? as u8;
            dev.set_profile_color(p, [r, g, b])?;
            dev.activate_profile(p)?;
            println!("perfil {p}: color = {r},{g},{b}");
        }
        "key" => {
            let p = num(&args[1], (NPROFILES - 1) as u32).ok_or("perfil invalido")? as u8;
            let idx = k535::key_by_name(&args[2])
                .or_else(|| args[2].parse::<u16>().ok())
                .ok_or("tecla desconocida")?;
            let r = num(&args[3], 255).ok_or("r invalido")? as u8;
            let g = num(&args[4], 255).ok_or("g invalido")? as u8;
            let b = num(&args[5], 255).ok_or("b invalido")? as u8;
            dev.set_key_color(p, idx, [r, g, b])?;
            println!("perfil {p}: tecla {idx} = {r},{g},{b}");
        }
        "raw" => {
            let mut req = [0u8; 64];
            for (i, a) in args[1..].iter().take(64).enumerate() {
                req[i] = u8::from_str_radix(a.trim_start_matches("0x"), 16)
                    .map_err(|_| Error::NotFound)?;
            }
            let sum: u16 = req[3..].iter().fold(0u16, |a, &b| a.wrapping_add(b as u16));
            req[1] = (sum & 0xff) as u8;
            req[2] = (sum >> 8) as u8;
            let resp = dev.command(req[3], [req[4], req[5], req[6], req[7]], &req[8..])?;
            hexdump("respuesta:", &resp);
        }
        "modes" => {
            for (i, n) in MODE_NAMES.iter().enumerate() {
                if !n.is_empty() {
                    println!("{i:2}  {n}");
                }
            }
        }
        _ => usage(),
    }
    Ok(())
}
