# k535ctl - control RGB del teclado Redragon K535-KB ("Mitra") en Linux

Controlador nativo para la iluminacion del teclado Redragon K535-KB
(Mitra, USB `0c45:5004`), **sin** la aplicacion de Windows y sin usar HID
"feature reports": el protocolo se implementa tal cual lo usa el software del
fabricante.

El detalle completo del protocolo esta en [RE-NOTES.md](RE-NOTES.md).

## Dos implementaciones

| Componente | Lenguaje | Para que sirve |
|---|---|---|
| `k535ctl` | C | linea de comandos, depuracion, scripts |
| `k535-gui` | Rust + GTK4 | interfaz grafica (perfiles, efectos, color por tecla) |
| `rust/src/lib.rs` | Rust | la libreria del protocolo, reutilizable |

La GUI usa el mismo protocolo directamente sobre `hidraw` (no llama al
binario en C): toda la comunicacion HID va en un hilo aparte, de modo que la
interfaz nunca se bloquea.

## Requisitos

* Linux con `hidraw` (modulo `hid-generic`, ya presente en la mayoria de
  distribuciones).
* Permiso de escritura sobre el nodo `hidraw` de la interfaz de fabricante.
* Para la GUI: Rust (>= 1.80) y GTK4 con cabeceras de desarrollo
  (`gtk4` >= 4.10, `glib` >= 2.74, `pango` >= 1.50).

## Instalacion

```sh
sudo ./install.sh
```

Instala `k535ctl` en `/usr/local/bin`, compila e instala la GUI Rust
`k535-gui` (si hay `cargo` en el PATH) y copia la regla udev
`/etc/udev/rules.d/60-redragon-k535.rules`, que da acceso al usuario activo
(`uaccess`) a la interfaz correcta y crea el enlace `/dev/redragon-k535`.

Solo la CLI, sin GUI:

```sh
sudo ./install.sh --no-gui
```

Desenchufa y vuelve a enchufar el teclado (o ejecuta
`sudo udevadm trigger --subsystem-match=hidraw`) para que se aplique la regla.

## Uso

```sh
k535ctl list                        # interfaces hidraw y cual es la del teclado
k535ctl keys                        # tabla indice de tecla -> nombre
k535ctl dump                        # vuelca los 3 perfiles y la config activa

k535ctl profile 1                   # activa el perfil 1
k535ctl mode 0 6                    # efecto "Normally on" en el perfil 0
k535ctl brightness 0 20             # brillo 0..25
k535ctl speed 0 3                   # velocidad 0..5
k535ctl color 0 255 0 0             # color RGB personalizado
k535ctl on 0 1                      # encender (0 = apagar)
k535ctl key 0 A 255 0 0             # color por tecla
k535ctl key 0 64 255 0 0            # lo mismo, por indice numerico
```

### Interfaz grafica

```sh
k535-gui
```

La GUI muestra:

* los **3 perfiles** del firmware (se activan al seleccionarlos),
* el **efecto** (los 20 modos del fabricante), **brillo**, **velocidad** y el
  interruptor de **encendido**,
* un selector de **color** que se puede aplicar al perfil entero o a todas las
  teclas,
* un **teclado 100% dibujado** (bloque principal + cluster de navegación +
  teclado numérico, distribución US QWERTY), con las posiciones reales
  tomadas de `Main.ini`; al pulsar cualquier tecla se abre el selector de
  color para esa tecla concreta.  Las 90 teclas del bloque principal tienen
  índices verificados; las del bloque extendido usan índices estimados
  (marcados en el tooltip) — dime si alguna no corresponde y la corrijo.

## Compilar la GUI a mano

```sh
cd rust
cargo build --release
./target/release/k535-gui
```

Los efectos disponibles (1..20) son los del fabricante:

```
1 Go with the stream    6 Normally on           11 Flowers blooming   16 Both ways
2 Clouds fly            7 Pass without trace    12 Swift action       17 Surmount
3 Winding paths         8 Ripple graff          13 Hurricane          18 Fast and the Furious
4 The trial of light    9 Fast run without trace 14 Accumulate        20 Coastal
5 Breathing             10 Snow winter jasmine  15 Digital Times
```

## Como funciona

* El teclado expone dos interfaces HID.  La interfaz 0 es el teclado boot
  (8 bytes) y la interfaz 1 es el canal de control de LEDs (64 bytes).
  `k535ctl` localiza la correcta por su descriptor de informe (coleccion de
  fabricante `0xFF1C/0x92`), sin depender del numero de `hidraw`.
* Los informes son de 64 bytes con `Report ID 4`, checksum de 16 bits en los
  bytes 1..2 y 56 bytes de payload en los bytes 8..63.
* Los perfiles ocupan 42 bytes cada uno en `perfil * 0x2a`; los colores por
  tecla viven en `perfil * 512 + indice * 3`.
* Todas las escrituras se envuelven en una transaccion `BEGIN` / `COMMIT`.

## Depuracion

```sh
k535ctl raw 04 01 00 00 00 00 00   # informe crudo (el checksum se recalcula)
```

Para ver el trafico del fabricante en Linux, `sniff`/`usbmon` o simply
`strace -e read,write k535ctl dump` en un terminal con el binario de depuracion.

## Diagnostico

* `no se encontro el teclado ...` -> el teclado no esta conectado, o el
  modulo `hid-generic` no cargo la interfaz de fabricante.
* `no se puede abrir /dev/hidrawN: Permission denied` -> falta la regla udev
  (reinstala y reenchufa el teclado) o no estas en la sesion grafica.
* `timeout esperando respuesta` -> el teclado esta ocupado con otra aplicacion
  (por ejemplo el software de Windows bajo Wine) o el cable/Hub pierde energia
  para el LED.
