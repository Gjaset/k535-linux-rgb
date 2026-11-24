# Redragon K535-KB ("Mitra") - notas de ingenieria inversa

Documento de referencia del protocolo usado por `k535ctl`.
Todo lo que sigue se obtuvo de forma estatica a partir del ejecutable de control
del fabricante (nada de esto se ha copiado de su codigo; solo se documenta el
protocolo de cara a interoperar con el hardware).

## 1. Material de partida

| Fichero | Descripcion |
|---|---|
| `~/Downloads/K535-KB - Software.exe` | instalador original (InstallShield, PE32, 6 345 728 B) |
| `extract/full4/DefaultComponent/Redragon  K535-KB  MECHANICAL GAMING KEYBOARD.exe` | aplicacion de control real (PE32 i386, MFC) |
| `extract/full4/Files1/Main.ini` | tabla de disposicion (UTF-16LE) + `[Mode]` + `[KEYCOLOR]` |
| `extract/full4/Files1/UserDengColor_*.userd` | colores por tecla de los perfiles (ANSI INI) |

Checksums del instalador:

```
MD5    4f9e113bde1776a4cfa3c46cfdc50423
SHA256 d221d050ae433d38ac6f5559a2c92439ceebb19e07b9a3313051691892a79f77
```

La aplicacion importa `HID.DLL` y `SETUPAPI.dll` directamente, no usa
`HidD_SetFeature`: escribe con `WriteFile()` sobre el handle HID, y enumera
los dispositivos con `SetupDiGetClassDevsW()` / `HidD_GetPreparsedData()` /
`HidD_GetAttributes()`.  No hay VID/PID codificados en el binario.

## 2. Extraccion del instalador

El instalador es un InstallShield multi-volumen.  Volumenes dentro del `.exe`:

```
data1.hdr  offset 1160646
data1.cab  offset  675881
data2.cab  offset 1183699
```

Se extraen con `unshield` (recompilado en `tools/unshield/`) colocando los tres
ficheros en el mismo directorio.  Resultado: 122 ficheros en `extract/full4/`.

## 3. Interfaz HID del teclado

```
0c45:5004  Microdia / SONiX  Redragon Mitra RGB Keyboard
```

Dos interfaces HID:

| Interfaz | hidraw | Descripcion |
|---|---|---|
| 0 | `hidraw2` | teclado boot, informes de 8 bytes |
| 1 | `hidraw3` | canal de control del fabricante, informes de 64 bytes |

Descriptor de informe de la interfaz 1 (113 bytes):

```
05 01 09 06 a1 01 85 01 ...        Report ID 1: teclado
05 01 09 80 a1 01 85 02 ...        Report ID 2: raton
05 0c 09 01 a1 01 85 03 ...        Report ID 3: input 16 bit
06 1c ff 09 92 a1 01 85 04
   19 00 2a ff 00 15 00 26 ff 00 75 08 95 3f 91 00   INPUT  63 bytes
   19 00 29 ff 81 00                                  OUTPUT 63 bytes
c0
```

* Usage page de fabricante `0xFF1C`, usage `0x92`.
* `Report ID 4`; con el byte de ID son **64 bytes** por informe.
* Hay informe de **entrada** (respuesta) y de **salida** (ordenes), ambos de
  63 bytes de datos.

En Linux el nodo correcto es el que tiene la coleccion `06 1c ff 09 92` en su
`report_descriptor`; `k535ctl` lo busca solo (ver `k535ctl list`).

## 4. Formato de los informes

```
buf[0]      = 0x04                       Report ID
buf[1..2]   = u16 little-endian           checksum = suma(buf[3..63])
buf[3]      = comando
buf[4..7]   = argumentos (4 bytes)
buf[8..63]  = payload (hasta 56 bytes)
```

El checksum se calcula **despues** de rellenar el resto del informe, sumando los
bytes 3..63 (61 bytes) con acumulador de 16 bits; el byte bajo va a `buf[1]` y
el alto a `buf[2]`.

Respuesta (informe de entrada con el mismo Report ID):

```
resp[0]     = 0x04
resp[3]     = comando
resp[7]     = estado      (0xff = error, el firmware responde -0x66)
resp[8..63] = datos devueltos
```

El payload maximo de una respuesta son 56 bytes (`resp[8..63]`); por eso la
aplicacion del fabricante lee la memoria en trozos de 56 bytes.

## 5. Tabla de comandos

| Cmd | Nombre | Argumentos (`buf[4..7]`) | Payload | Descripcion |
|---|---|---|---|---|
| 0x01 | BEGIN | 0 | - | abre transaccion |
| 0x02 | COMMIT | 0 | - | cierra transaccion (`Sleep(10)`) |
| 0x03 | READ_CFG | `count=0x2c` | - | lee 44 bytes de configuracion activa |
| 0x04 | WRITE_CFG | `count=0x2c` | 44 bytes | escribe 44 bytes de configuracion activa |
| 0x05 | READ_MEM | `[0]=count [1]=addr_lo [2]=addr_hi [3]=0` | - | lee `count` bytes en `addr` |
| 0x06 | WRITE_MEM | `[0]=count [1]=addr_lo [2]=addr_hi [3]=0` | `count` bytes | escribe en `addr` |
| 0x07 | - | `count>=3` | - | sin uso (cuerpo no descifrado) |
| 0x08 | - | `count>=3` | - | sin uso (cuerpo no descifrado) |
| 0x09 | - | base,count | datos | escritura por bloques (0x38 bytes) |
| 0x0a | - | ptr,count | datos | escritura de blob grande |
| 0x0b..0x0e | - | 0 | - | sin referencias en el codigo (codigo muerto) |
| 0x0f | - | base,count | datos | escritura por bloques |
| 0x10 | WRITE_KEY | `[0]=count [1]=addr_lo [2]=addr_hi` | datos | color de tecla, direccion explicita |
| 0x11 | SET_KEY | `[0]=count [1]=addr_lo [2]=addr_hi` | datos | color de tecla, perfil activo |

Nota: el comando 0x10 **no** copia payload; el 0x11 si.  La aplicacion usa
`0x01 -> 0x11 -> 0x02` para cambiar el color de una tecla.

## 6. Memoria del dispositivo

### 6.1 Bloque de perfil (42 bytes)

Direccion base: `perfil * 0x2a` (`0x00`, `0x2a`, `0x54`).  Son tres perfiles.

| Offset | Contenido | Rango / notas |
|---|---|---|
| +0x00 | modo de iluminacion | 1..20 (nombres en `Main.ini [Mode]`) |
| +0x01 | brillo | 0..25 (control 1178 de la ventana) |
| +0x02 | velocidad | 0..5 (control 1179 de la ventana) |
| +0x03 | reservado | - |
| +0x04 | encendido | 0/1 |
| +0x05..+0x07 | color RGB personalizado | R,G,B |
| +0x0a | indice de perfil | 0..2 (asi se cambia de perfil) |
| +0x0f | direccion de la animacion | 0..3 (acotado en el handler) |
| +0x11 | campo | - |
| +0x12, +0x13 | flags | - |
| +0x19..+0x1b | color RGB | R,G,B |
| +0x1c | flag | - |
| +0x1e..+0x20 | color RGB | R,G,B |
| +0x21 | flag | - |

Confirmado por el handler de la ventana: al mover el control 1178 (brillo) el
programa hace

```
mov dword [0x7739bc], valor      ; -> perfil + 0x01
call 0x440910                    ; BEGIN
imul eax, [perfil], 0x2a
inc  eax                         ; direccion = perfil*0x2a + 1
... 0x440700 (WRITE_MEM, 1 byte) ... 0x440320 (COMMIT)
```

y para el control 1179 (velocidad) exactamente igual pero con `+2`.

### 6.2 Configuracion activa (44 bytes)

Se lee con el comando 0x03 y se escribe con el 0x04.  El byte `+0x0a` es el
**indice del perfil activo**: la aplicacion lee la configuracion, cambia ese
byte y la vuelve a escribir para cambiar de perfil.

### 6.3 Colores por tecla

Viven en un espacio de memoria distinto al de los perfiles.  **Direccion
absoluta:**

```
direccion = 0x0580 + perfil * 512 + indice_de_tecla * 3     (3 bytes R,G,B)
```

El comando `0x11` (SET_KEY) interpreta su argumento de direccion como un
**offset relativo a 0x0580**, por eso la aplicacion pasa
`perfil*512 + indice*3`.  En cambio el comando `0x05` (READ_MEM) necesita la
direccion absoluta: leer desde `perfil*512` devuelve los bloques de perfil y
una tabla de codigos HID, no los colores.

Confirmado experimentalmente escribiendo un color conocido y comparando el
volcado antes/después:

```
perfil 0, indice  0 -> 0x580
perfil 0, indice  1 -> 0x583
perfil 0, indice 14 -> 0x5aa
perfil 1, indice  0 -> 0x780
perfil 2, indice  0 -> 0x980
```

Se escriben con `0x01 -> 0x11 -> 0x02` (la aplicacion escribe 3 bytes, R,G,B,
en `perfil*512 + indice*3`).

## 7. Numeros de tecla

Hay tres espacios de identificacion distintos:

1. **Indice de tecla del dispositivo** (0..132): el que aparece en el protocolo
   y en los ficheros `UserDengColor_*.userd`.  Es el que usa `k535ctl`.
2. **"app id"** (1010..1200): identificador interno de la aplicacion.
3. `Main.ini`: `KEY_xxx` (bitmap de la ventana) y la tabla numerica (nombre +
   posicion).

Los dos ultimos estan relacionados por tablas internas del binario:

```
0x6cae08 : 106 indices de tecla del dispositivo
0x6cf8c8 : los 106 "app id" correspondientes
0x6cafb0 : 9 entradas (todas con indice 125)
0x6cf8a4 : sus app id (1000..1004, 1140..1143)
```

Ordenando por "app id" se obtiene el orden fisico del teclado, y las tablas
coinciden con la tabla numerica de `Main.ini`:

```
app 1022 -> indice   0 = Esc        app 1030..1041 -> indice 22..33 = 1..+
app 1056 -> indice  42 = Tab        app 1044..1055 -> indice 43..54 = Q..]
app 1069 -> indice  63 = CapsLock   app 1058..1068 -> indice 64..74 = A..'
app 1082 -> indice  84 = Shift      app 1071..1080 -> indice 86..95 = X..RShift
app 1090 -> indice 108 = Space      app 1083..1088 -> indices 105..111 = Ctrl..App
app 1091..1102 -> indices 14,15,16,35,36,37,56,57,58,119,120,121 = F1..F12
```

Corroboracion independiente: el perfil "FPS" de `UserDengColor_0.userd` pinta
los indices 22..28 (teclas 1..7) en amarillo, 43..45 (Q,W,E) en verde, 64..66
(A,S,D) en colores distintos y 67 (F) en naranja: exactamente el juego de teclas
habitual de un preset FPS, lo que confirma la tabla.

Los indices que no tienen nombre fiable en `Main.ini` (1..12, 21, 38..41, 76,
80..82, 96, 97, 99, 101, 103, 104, 113, 123..125) se pueden usar numericamente
con `k535ctl key <perfil> <indice> ...` y serviran para calibrarlos a mano.

## 8. Nombres de los modos de iluminacion

Del apartado `[Mode]` de `Main.ini`:

```
1  Go with the stream        11 Flowers blooming
2  Clouds fly                12 Swift action
3  Winding paths             13 Hurricane
4  The trial of light        14 Accumulate
5  Breathing                 15 Digital Times
6  Normally on               16 Both ways
7  Pass without trace        17 Surmount
8  Ripple graff              18 Fast and the Furious
9  Fast run without trace    20 Coastal
10 Snow winter jasmine
```

## 9. Estado y pasos pendientes

* [x] Protocolo de informes, comandos, checksum.
* [x] Mapa de memoria de perfiles y colores por tecla.
* [x] Localizacion de la interfaz HID correcta en Linux (`hidraw3`).
* [x] `k535ctl` implementado (C, sin dependencias).
* [ ] Validar en hardware: leer (`dump`), cambiar modo/brillo/color y confirmar
      que el teclado responde (requiere acceso de escritura a `hidraw3`).
* [ ] Calibrar los indices de tecla sin nombre.
* [ ] Confirmar si el firmware guarda los cambios en memoria no volatil (el
      `COMMIT` deberia encargarse de ello).

## 10. Geometria real del teclado y tabla de 106 teclas

`Main.ini [BUTTONINFO]` contiene las coordenadas y tamano de las **104 teclas
fisicas** (incluido el teclado numerico y el cluster de navegacion), en la
imagen `Skins/main_normal.jpg`.  Es la fuente usada para dibujar el teclado
100% en la GUI.

La tabla numerica de `Main.ini` (indice de dispositivo -> nombre) solo cubre
las 78 teclas del bloque principal; el resto de nombres estan en `BUTTONINFO`
por scan code.

En el ejecutable hay dos tablas paralelas de 106 entradas (u32):

```
VA 0x6cae08  -> indices de dispositivo
VA 0x6cf8c8  -> "app id" de la aplicacion
VA 0x6cafb0  -> 9 entradas con indice 125 (teclas multimedia)
VA 0x6cf8a4  -> sus app id (1000..1004, 1140..1143)
```

El emparejamiento por app id permite nombrar teclas que no estaban en la
tabla numerica, por ejemplo:

```
app 1029 -> indice  21  (` / ~)
app 1081 -> indice  97
app 1089 -> indice 113
app 1103 -> indice  99
app 1104..1107 -> indices 38..41
app 1108..1111 -> indices 59..62
app 1112..1114 -> indices 80..82
app 1115..1118 -> indices 101..104
app 1119 -> 124, app 1120 -> 123
app 1010..1015 -> 1..6, app 1023..1028 -> 7..12  (capa Fn/multimedia)
```

Los indices del bloque extendido (numpad, navegacion, PrtSc/ScrLk/Pause)
**no han sido calibrados en hardware**: la GUI los dibuja mas oscuros y no les
asigna color.  Para calibrarlos basta con escribir un color a cada indice
sospechoso con `k535ctl key <perfil> <indice> R G B` y observar que tecla se
ilumina.

## 11. Calibracion de las teclas del bloque extendido

El dispositivo expone una tabla de 126 registros de 3 bytes por perfil en
`0x80 + perfil*0x17c`, cuyo tercer byte es un **usage HID** (0x29=Esc,
0x35=grave, 0x62=Num0, 0x63=Num., 0x58=NumEnter, etc.).  El orden de la tabla
es columna-mayor (fila de funcion, numero, Q, A, Z, bloque inferior; luego
navegacion y numpad) y se repite cada 126 entradas.

Calibracion en hardware (escribiendo un color a un indice y observando la
tecla fisica):

```
indice 113 = Ctrl derecho
indice 122 = Num. (punto decimal del numpad)
indice 123 = Num0
```

El resto de indices del bloque extendido se han asignado de forma
**estimada** a partir de la tabla de 106 pares app-id/indice del apartado 10:
numpad 99/38/39/40/98/60/61/62/80/81/82/101/102/103/124/123/122,
navegacion 41/96/97/76/104/125, flechas 114..117, PrtSc/ScrLk/Pause 130..132.
Las teclas con indice estimado se marcan como tal en el tooltip de la GUI.

Nota: los efectos animados (p. ej. Coastal) y el modo 6 "Normally on" (color
global) no permiten ver los colores individuales; para calibrar hay que
observar el cambio inmediato al escribir el color en el indice candidato.
