/* SPDX-License-Identifier: GPL-3.0-or-later */
/*
 * k535ctl - Control de iluminacion RGB del teclado Redragon K535-KB ("Mitra")
 *           en Linux, sin la aplicacion de Windows.
 *
 * Protocolo inversado del ejecutable de control del fabricante:
 *   "Redragon  K535-KB  MECHANICAL GAMING KEYBOARD.exe"
 *   (MFC/Win32, escribe con WriteFile() sobre el handle HID).
 *
 * Informe HID: 64 bytes, Report ID 4 (coleccion de fabricante 0xFF1C/0x92).
 *   buf[0]      = 0x04                       (Report ID)
 *   buf[1..2]   = u16 LE  suma(buf[3..63])   (checksum)
 *   buf[3]      = comando
 *   buf[4..7]   = argumentos (4 bytes)
 *   buf[8..63]  = payload (hasta 56 bytes)
 *
 * La respuesta llega como informe de entrada del mismo Report ID:
 *   resp[7]     = estado (0xff = error)
 *   resp[8..]   = datos devueltos
 *
 * Memoria del dispositivo (lectura/escritura via cmd 0x05/0x06):
 *   direccion 0x00 + perfil*0x2a : bloque de perfil de 42 bytes
 *     +0x00 modo de iluminacion (1..20)     +0x01 brillo (0..25)
 *     +0x02 velocidad (0..5)                +0x04 encendido (0/1)
 *     +0x05..07 color RGB personalizado      +0x0a indice de perfil
 *     +0x0f direccion (0..3)                +0x19..1b RGB   +0x1e..20 RGB
 *   Los colores por tecla viven en otro espacio, escrito con cmd 0x11:
 *     direccion = perfil*512 + indice_de_tecla*3  (3 bytes R,G,B)
 */

#define _GNU_SOURCE
#include <ctype.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define REPORT_LEN      64
#define MAX_PAYLOAD     56          /* resp[8..63] */
#define HID_VENDOR      0x0c45
#define HID_PRODUCT     0x5004

/* comandos */
enum {
	CMD_BEGIN     = 0x01,
	CMD_COMMIT    = 0x02,
	CMD_READ_CFG  = 0x03,
	CMD_WRITE_CFG = 0x04,
	CMD_READ_MEM  = 0x05,
	CMD_WRITE_MEM = 0x06,
	CMD_SET_KEY   = 0x11
};

/* bloque de perfil */
#define PROF_STRIDE   0x2a
#define P_MODE        0x00
#define P_BRIGHT      0x01
#define P_SPEED       0x02
#define P_ON          0x04
#define P_COLOR       0x05          /* R,G,B */
#define P_PROFILE_IDX 0x0a
#define P_DIRECTION   0x0f
#define NPROFILES     3

/* color por tecla */
#define KEYMAP_STRIDE 512           /* perfil * 512 + indice * 3 (offset de 0x11) */
#define KEYMAP_BASE   0x0580        /* base absoluta para lecturas con 0x05 */

/* ------------------------------------------------------------------ */

struct keydef {
	unsigned idx;
	const char *name;
};

/*
 * Nombres verificados de la tabla de disposicion del fabricante
 * (Main.ini, tabla numerica) y de la fila F1-F12 (orden de app-id).
 * Los indices no listados se pueden usar numericamente.
 */
static const struct keydef keymap[] = {
	{0,   "Esc"},
	{14,  "F1"}, {15,  "F2"}, {16,  "F3"},
	{22,  "1"}, {23,  "2"}, {24,  "3"}, {25,  "4"}, {26,  "5"},
	{27,  "6"}, {28,  "7"}, {29,  "8"}, {30,  "9"}, {31,  "0"},
	{32,  "-"}, {33,  "+"}, {34,  "BackSpace"},
	{35,  "F4"}, {36,  "F5"}, {37,  "F6"},
	{42,  "Tab"},
	{43,  "Q"}, {44,  "W"}, {45,  "E"}, {46,  "R"}, {47,  "T"},
	{48,  "Y"}, {49,  "U"}, {50,  "I"}, {51,  "O"}, {52,  "P"},
	{53,  "["}, {54,  "]"}, {55,  "\\"},
	{56,  "F7"}, {57,  "F8"}, {58,  "F9"},
	{63,  "CapsLock"},
	{64,  "A"}, {65,  "S"}, {66,  "D"}, {67,  "F"}, {68,  "G"},
	{69,  "H"}, {70,  "J"}, {71,  "K"}, {72,  "L"},
	{73,  ";"}, {74,  "'"},
	{75,  "Enter"},
	{84,  "Shift"},
	{85,  "Z"}, {86,  "X"}, {87,  "C"}, {88,  "V"}, {89,  "B"},
	{90,  "N"}, {91,  "M"}, {92,  ","}, {93,  "."}, {94,  "/"},
	{95,  "RightShift"},
	{102, "K45"},
	{105, "Ctrl"}, {106, "Win"}, {107, "Alt"}, {108, "Space"},
	{109, "RightAlt"}, {110, "Fn"}, {111, "App"}, {112, "RightCtrl"},
	{119, "F10"}, {120, "F11"}, {121, "F12"},
};

static const char *mode_names[] = {
	NULL,
	"Go with the stream", "Clouds fly", "Winding paths",
	"The trial of light", "Breathing", "Normally on",
	"Pass without trace", "Ripple graff", "Fast run without trace",
	"Snow winter jasmine", "Flowers blooming", "Swift action",
	"Hurricane", "Accumulate", "Digital Times", "Both ways",
	"Surmount", "Fast and the Furious", NULL, "Coastal"
};

static void hexdump(const char *tag, const uint8_t *d, size_t n)
{
	size_t i;
	printf("%-18s", tag);
	for (i = 0; i < n; i++) {
		if (i && !(i % 16))
			printf("\n%-18s", "");
		printf(" %02x", d[i]);
	}
	printf("\n");
}

/* ------------------------------------------------------------------ */
/* descubrimiento del dispositivo                                      */

static int read_file(const char *path, uint8_t *buf, size_t max)
{
	int fd = open(path, O_RDONLY);
	ssize_t n;
	if (fd < 0)
		return -1;
	n = read(fd, buf, max);
	close(fd);
	return n < 0 ? -1 : (int)n;
}

static int hidraw_is_redragon(const char *dir)
{
	char path[640];
	uint8_t buf[4096];
	int n, i;

	snprintf(path, sizeof path, "%s/device/uevent", dir);
	n = read_file(path, buf, sizeof buf - 1);
	if (n <= 0)
		return 0;
	buf[n] = 0;
	if (!strstr((char *)buf, "HID_ID=0003:00000C45:00005004"))
		return 0;

	snprintf(path, sizeof path, "%s/device/report_descriptor", dir);
	n = read_file(path, buf, sizeof buf);
	if (n < 8)
		return 0;
	/* coleccion de fabricante: usage page 0xFF1C, usage 0x92 */
	for (i = 0; i + 4 < n; i++) {
		if (buf[i] == 0x06 && buf[i+1] == 0x1c && buf[i+2] == 0xff &&
		    buf[i+3] == 0x09 && buf[i+4] == 0x92)
			return 1;
	}
	return 0;
}

static int find_device(char *out, size_t outlen)
{
	const char *env = getenv("K535_HIDRAW");
	DIR *d;
	struct dirent *de;
	char best[512] = "";
	int found = 0;

	if (env && *env) {
		if (access(env, R_OK | W_OK) != 0) {
			fprintf(stderr, "K535_HIDRAW=%s: %s\n", env, strerror(errno));
			return -1;
		}
		snprintf(out, outlen, "%s", env);
		return 0;
	}

	d = opendir("/sys/class/hidraw");
	if (!d) {
		fprintf(stderr, "no se puede abrir /sys/class/hidraw: %s\n", strerror(errno));
		return -1;
	}
	while ((de = readdir(d))) {
		char dir[512], dev[512];
		if (strncmp(de->d_name, "hidraw", 6))
			continue;
		snprintf(dir, sizeof dir, "/sys/class/hidraw/%s", de->d_name);
		if (!hidraw_is_redragon(dir))
			continue;
		snprintf(dev, sizeof dev, "/dev/%s", de->d_name);
		if (!found || strcmp(dev, best) < 0) {
			snprintf(best, sizeof best, "%s", dev);
			found = 1;
		}
	}
	closedir(d);
	if (!found) {
		fprintf(stderr, "no se encontro el teclado Redragon K535-KB "
				"(0c45:5004) con la interfaz de fabricante\n");
		return -1;
	}
	snprintf(out, outlen, "%s", best);
	return 0;
}

static void list_devices(void)
{
	DIR *d = opendir("/sys/class/hidraw");
	struct dirent *de;
	if (!d) {
		fprintf(stderr, "no se puede abrir /sys/class/hidraw: %s\n", strerror(errno));
		return;
	}
	printf("%-22s %-38s %s\n", "DISPOSITIVO", "HID_ID", "INTERFAZ FABRICANTE");
	while ((de = readdir(d))) {
		char dir[512], path[640], id[256] = "?";
		uint8_t buf[4096];
		int n;
		if (strncmp(de->d_name, "hidraw", 6))
			continue;
		snprintf(dir, sizeof dir, "/sys/class/hidraw/%s", de->d_name);
		snprintf(path, sizeof path, "%s/device/uevent", dir);
		n = read_file(path, buf, sizeof buf - 1);
		if (n > 0) {
			buf[n] = 0;
			char *p = strstr((char *)buf, "HID_ID=");
			if (p) {
				size_t l = strcspn(p, "\n");
				if (l < sizeof id) {
					memcpy(id, p, l);
					id[l] = 0;
				}
			}
		}
		printf("/dev/%-14s %-38s %s\n", de->d_name, id,
		       hidraw_is_redragon(dir) ? "si" : "no");
	}
	closedir(d);
}

/* ------------------------------------------------------------------ */
/* transporte                                                           */

static int hid_send(int fd, const uint8_t *req, uint8_t *resp)
{
	struct pollfd pfd;
	ssize_t got;

	if (write(fd, req, REPORT_LEN) != REPORT_LEN) {
		fprintf(stderr, "write(hidraw): %s\n", strerror(errno));
		return -1;
	}
	pfd.fd = fd;
	pfd.events = POLLIN;
	pfd.revents = 0;
	if (poll(&pfd, 1, 1000) <= 0) {
		fprintf(stderr, "timeout esperando respuesta del teclado\n");
		return -2;
	}
	got = read(fd, resp, REPORT_LEN);
	if (got <= 0) {
		fprintf(stderr, "read(hidraw): %s\n", got == 0 ? "EOF" : strerror(errno));
		return -3;
	}
	if (got < REPORT_LEN)
		memset(resp + got, 0, REPORT_LEN - got);
	return 0;
}

/* envia un comando; devuelve 0 ok, -102 error del dispositivo */
static int cmd_tx(int fd, uint8_t cmd, const uint8_t args[4],
		  const uint8_t *payload, size_t plen, uint8_t *resp)
{
	uint8_t req[REPORT_LEN];
	uint16_t sum = 0;
	int i, rc;

	if (plen > MAX_PAYLOAD)
		return -1;
	memset(req, 0, sizeof req);
	req[0] = 0x04;
	req[3] = cmd;
	if (args)
		memcpy(req + 4, args, 4);
	if (plen && payload)
		memcpy(req + 8, payload, plen);
	for (i = 3; i < REPORT_LEN; i++)
		sum += req[i];
	req[1] = sum & 0xff;
	req[2] = sum >> 8;

	rc = hid_send(fd, req, resp);
	if (rc)
		return rc;
	if (resp[7] == 0xff) {
		fprintf(stderr, "el teclado rechazo el comando 0x%02x\n", cmd);
		return -102;
	}
	return 0;
}

static int mem_read(int fd, uint16_t addr, uint8_t *out, size_t len)
{
	size_t done = 0;

	while (done < len) {
		size_t chunk = len - done;
		uint8_t args[4], resp[REPORT_LEN];
		int rc;

		if (chunk > MAX_PAYLOAD)
			chunk = MAX_PAYLOAD;
		args[0] = (uint8_t)chunk;
		args[1] = (uint8_t)((addr + done) & 0xff);
		args[2] = (uint8_t)((addr + done) >> 8);
		args[3] = 0;
		rc = cmd_tx(fd, CMD_READ_MEM, args, NULL, 0, resp);
		if (rc)
			return rc;
		memcpy(out + done, resp + 8, chunk);
		done += chunk;
	}
	return 0;
}

static int mem_write(int fd, uint16_t addr, const uint8_t *data, size_t len)
{
	size_t done = 0;

	while (done < len) {
		size_t chunk = len - done;
		uint8_t args[4], resp[REPORT_LEN];
		int rc;

		if (chunk > MAX_PAYLOAD)
			chunk = MAX_PAYLOAD;
		args[0] = (uint8_t)chunk;
		args[1] = (uint8_t)((addr + done) & 0xff);
		args[2] = (uint8_t)((addr + done) >> 8);
		args[3] = 0;
		rc = cmd_tx(fd, CMD_WRITE_MEM, args, data + done, chunk, resp);
		if (rc)
			return rc;
		done += chunk;
	}
	return 0;
}

static int cfg_read(int fd, uint8_t *out44)
{
	uint8_t args[4] = {44, 0, 0, 0};
	uint8_t resp[REPORT_LEN];
	int rc = cmd_tx(fd, CMD_READ_CFG, args, NULL, 0, resp);
	if (rc)
		return rc;
	memcpy(out44, resp + 8, 44);
	return 0;
}

static int cfg_write(int fd, const uint8_t *in44)
{
	uint8_t args[4] = {44, 0, 0, 0};
	uint8_t resp[REPORT_LEN];
	return cmd_tx(fd, CMD_WRITE_CFG, args, in44, 44, resp);
}

/* ------------------------------------------------------------------ */
/* operaciones de alto nivel                                            */

static int profile_activate(int fd, int p)
{
	uint8_t cfg[44];
	int rc = cfg_read(fd, cfg);
	if (rc)
		return rc;
	cfg[P_PROFILE_IDX] = (uint8_t)p;
	rc = cfg_write(fd, cfg);
	if (rc)
		return rc;
	rc = cfg_read(fd, cfg);
	if (rc)
		return rc;
	if (cfg[P_PROFILE_IDX] != p) {
		fprintf(stderr, "el teclado no cambio al perfil %d (sigue en %u)\n",
			p, cfg[P_PROFILE_IDX]);
		return -1;
	}
	return 0;
}

static int profile_field_set(int fd, int p, unsigned off, uint8_t value)
{
	uint8_t resp[REPORT_LEN], args[4] = {0, 0, 0, 0};
	int rc;

	rc = cmd_tx(fd, CMD_BEGIN, args, NULL, 0, resp);
	if (rc)
		return rc;
	rc = mem_write(fd, (uint16_t)(p * PROF_STRIDE + off), &value, 1);
	if (rc)
		return rc;
	return cmd_tx(fd, CMD_COMMIT, args, NULL, 0, resp);
}

static int key_color_set(int fd, int p, int idx, uint8_t r, uint8_t g, uint8_t b)
{
	uint8_t resp[REPORT_LEN], args[4] = {0, 0, 0, 0};
	uint8_t rgb[3] = {r, g, b};
	uint16_t addr = (uint16_t)(p * KEYMAP_STRIDE + idx * 3);
	int rc;

	rc = cmd_tx(fd, CMD_BEGIN, args, NULL, 0, resp);
	if (rc)
		return rc;
	args[0] = 3;
	args[1] = (uint8_t)(addr & 0xff);
	args[2] = (uint8_t)(addr >> 8);
	args[3] = 0;
	rc = cmd_tx(fd, CMD_SET_KEY, args, rgb, 3, resp);
	if (rc)
		return rc;
	return cmd_tx(fd, CMD_COMMIT, args, NULL, 0, resp);
}

/* ------------------------------------------------------------------ */

static int key_lookup(const char *name)
{
	size_t i;

	for (i = 0; i < sizeof keymap / sizeof keymap[0]; i++) {
		if (strcasecmp(keymap[i].name, name) == 0)
			return (int)keymap[i].idx;
	}
	/* alias */
	if (!strcasecmp(name, "space")) return 108;
	if (!strcasecmp(name, "return")) return 75;
	if (!strcasecmp(name, "lshift")) return 84;
	if (!strcasecmp(name, "rshift")) return 95;
	if (!strcasecmp(name, "lctrl")) return 105;
	if (!strcasecmp(name, "rctrl")) return 112;
	if (!strcasecmp(name, "lalt")) return 107;
	if (!strcasecmp(name, "ralt")) return 109;
	if (!strcasecmp(name, "caps")) return 63;
	if (!strcasecmp(name, "bksp")) return 34;
	if (!strcasecmp(name, "win") || !strcasecmp(name, "super")) return 106;
	if (!strcasecmp(name, "menu")) return 111;
	if (!strcasecmp(name, "del") || !strcasecmp(name, "delete")) return 40;
	if (!strcasecmp(name, "ins") || !strcasecmp(name, "insert")) return 38;
	if (!strcasecmp(name, "home")) return 39;
	if (!strcasecmp(name, "end")) return 41;
	if (!strcasecmp(name, "pgup")) return 59;
	if (!strcasecmp(name, "pgdn")) return 60;
	if (!strcasecmp(name, "scroll")) return 99;
	if (!strcasecmp(name, "prtsc")) return 99;
	if (!strcasecmp(name, "left")) return 80;
	if (!strcasecmp(name, "right")) return 81;
	if (!strcasecmp(name, "down")) return 82;
	if (!strcasecmp(name, "up")) return 96;
	return -1;
}

static void usage(const char *p)
{
	printf("uso: %s <orden> [args]\n\n", p);
	printf("  list                                 lista interfaces hidraw\n");
	printf("  keys                                 tabla indice -> nombre de tecla\n");
	printf("  dump                                 vuelca perfiles y configuracion\n");
	printf("  dump-json                            lo mismo, en JSON (para la GUI)\n");
	printf("  keydump <perfil>                     vuelca el mapa de color por tecla\n");
	printf("  profile <0-2>                        activa un perfil\n");
	printf("  mode <perfil> <1-20>                 modo de iluminacion\n");
	printf("  brightness <perfil> <0-25>           brillo\n");
	printf("  speed <perfil> <0-5>                 velocidad de animacion\n");
	printf("  color <perfil> <r> <g> <b>           color RGB personalizado\n");
	printf("  on <perfil> <0|1>                    encender/apagar\n");
	printf("  key <perfil> <tecla|indice> <r> <g> <b>   color por tecla\n");
	printf("  raw <hex...>                         envia un informe crudo (depuracion)\n");
}

static int parse_uint(const char *s, unsigned long max, unsigned long *out)
{
	char *end;
	unsigned long v;

	if (!s || !*s)
		return -1;
	errno = 0;
	v = strtoul(s, &end, 0);
	if (errno || *end || v > max)
		return -1;
	*out = v;
	return 0;
}

int main(int argc, char **argv)
{
	char devpath[256];
	int fd, rc = 0;
	const char *cmd;

	if (argc < 2) {
		usage(argv[0]);
		return 2;
	}
	cmd = argv[1];

	if (!strcmp(cmd, "list")) {
		list_devices();
		return 0;
	}
	if (!strcmp(cmd, "keys")) {
		size_t i;
		printf("indice  nombre\n");
		for (i = 0; i < sizeof keymap / sizeof keymap[0]; i++)
			printf("%5u   %s\n", keymap[i].idx, keymap[i].name);
		return 0;
	}

	if (find_device(devpath, sizeof devpath) != 0)
		return 1;
	fd = open(devpath, O_RDWR);
	if (fd < 0) {
		fprintf(stderr, "no se puede abrir %s: %s\n", devpath, strerror(errno));
		fprintf(stderr, "permiso requerido: regla udev o ejecucion como root\n");
		return 1;
	}

	if (!strcmp(cmd, "dump")) {
		uint8_t blk[42], cfg[44];
		int p;

		for (p = 0; p < NPROFILES; p++) {
			if (mem_read(fd, (uint16_t)(p * PROF_STRIDE), blk, sizeof blk) != 0) {
				rc = 1;
				goto out;
			}
			printf("\n=== perfil %d (direccion 0x%02x) ===\n", p, p * PROF_STRIDE);
			hexdump("bloque:", blk, sizeof blk);
			{
				const char *mn = NULL;
				unsigned m = blk[P_MODE];
				if (m >= 1 && m < sizeof mode_names / sizeof mode_names[0])
					mn = mode_names[m];
				printf("  modo        : %u%s%s\n", m, mn ? " - " : "",
				       mn ? mn : "");
			}
			printf("  brillo      : %u\n", blk[P_BRIGHT]);
			printf("  velocidad   : %u\n", blk[P_SPEED]);
			printf("  encendido   : %u\n", blk[P_ON]);
			printf("  color RGB   : %u,%u,%u\n", blk[P_COLOR],
			       blk[P_COLOR + 1], blk[P_COLOR + 2]);
			printf("  indice prof.: %u\n", blk[P_PROFILE_IDX]);
			printf("  direccion   : %u\n", blk[P_DIRECTION]);
		}
		if (cfg_read(fd, cfg) == 0) {
			printf("\n=== configuracion activa (44 bytes) ===\n");
			hexdump("cfg:", cfg, sizeof cfg);
			printf("  perfil activo: %u\n", cfg[P_PROFILE_IDX]);
		}
	} else if (!strcmp(cmd, "dump-json")) {
		uint8_t blk[42], cfg[44];
		int p, have_cfg;

		printf("{\"device\":\"%s\",\"profiles\":[", devpath);
		for (p = 0; p < NPROFILES; p++) {
			if (mem_read(fd, (uint16_t)(p * PROF_STRIDE), blk, sizeof blk) != 0) {
				rc = 1;
				goto out;
			}
			printf("%s{\"index\":%d,\"mode\":%u,\"brightness\":%u,"
			       "\"speed\":%u,\"on\":%u,\"color\":[%u,%u,%u],"
			       "\"profile_id\":%u,\"direction\":%u,\"raw\":\"",
			       p ? "," : "", p, blk[P_MODE], blk[P_BRIGHT], blk[P_SPEED],
			       blk[P_ON], blk[P_COLOR], blk[P_COLOR + 1], blk[P_COLOR + 2],
			       blk[P_PROFILE_IDX], blk[P_DIRECTION]);
			for (size_t i = 0; i < sizeof blk; i++)
				printf("%02x", blk[i]);
			printf("\"}");
		}
		printf("]");
		have_cfg = (cfg_read(fd, cfg) == 0);
		if (have_cfg)
			printf(",\"active\":%u,\"active_raw\":\"", cfg[P_PROFILE_IDX]);
		else
			printf(",\"active\":-1,\"active_raw\":\"");
		if (have_cfg) {
			for (size_t i = 0; i < sizeof cfg; i++)
				printf("%02x", cfg[i]);
		}
		printf("\"}\n");
	} else if (!strcmp(cmd, "keydump")) {
		unsigned long p;
		uint8_t buf[512];
		if (argc != 3 || parse_uint(argv[2], NPROFILES - 1, &p)) {
			fprintf(stderr, "perfil debe ser 0..%d\n", NPROFILES - 1);
			rc = 2;
			goto out;
		}
		if (mem_read(fd, (uint16_t)(KEYMAP_BASE + p * KEYMAP_STRIDE), buf,
			     sizeof buf) != 0) {
			rc = 1;
			goto out;
		}
		printf("perfil %lu, mapa de teclas (%u bytes)\n", p, (unsigned)sizeof buf);
		for (size_t i = 0; i < sizeof buf; i += 16) {
			printf("%03zu: ", i);
			for (size_t j = 0; j < 16 && i + j < sizeof buf; j++)
				printf("%02x ", buf[i + j]);
			printf("\n");
		}
	} else if (!strcmp(cmd, "profile")) {
		unsigned long p;
		if (argc != 3 || parse_uint(argv[2], NPROFILES - 1, &p)) {
			fprintf(stderr, "perfil debe ser 0..%d\n", NPROFILES - 1);
			rc = 2;
		} else if (profile_activate(fd, (int)p) != 0) {
			rc = 1;
		} else {
			printf("perfil activo: %lu\n", p);
		}
	} else if (!strcmp(cmd, "mode") || !strcmp(cmd, "brightness") ||
		   !strcmp(cmd, "speed") || !strcmp(cmd, "on")) {
		unsigned long p, v, max;
		unsigned off;
		if (argc != 4 || parse_uint(argv[2], NPROFILES - 1, &p)) {
			fprintf(stderr, "perfil debe ser 0..%d\n", NPROFILES - 1);
			rc = 2;
			goto out;
		}
		if (!strcmp(cmd, "mode")) {
			off = P_MODE; max = 20;
		} else if (!strcmp(cmd, "brightness")) {
			off = P_BRIGHT; max = 25;
		} else if (!strcmp(cmd, "speed")) {
			off = P_SPEED; max = 5;
		} else {
			off = P_ON; max = 1;
		}
		if (parse_uint(argv[3], max, &v)) {
			fprintf(stderr, "valor debe ser 0..%lu\n", max);
			rc = 2;
			goto out;
		}
		if (profile_field_set(fd, (int)p, off, (uint8_t)v) != 0) {
			rc = 1;
			goto out;
		}
		if (profile_activate(fd, (int)p) != 0) {
			rc = 1;
			goto out;
		}
		printf("perfil %lu: %s = %lu\n", p, cmd, v);
	} else if (!strcmp(cmd, "color")) {
		unsigned long p, r, g, b;
		uint8_t rgb[3];
		uint8_t resp[REPORT_LEN], args[4] = {0, 0, 0, 0};

		if (argc != 6 || parse_uint(argv[2], NPROFILES - 1, &p) ||
		    parse_uint(argv[3], 255, &r) || parse_uint(argv[4], 255, &g) ||
		    parse_uint(argv[5], 255, &b)) {
			fprintf(stderr, "uso: %s color <perfil> <r> <g> <b>\n", argv[0]);
			rc = 2;
			goto out;
		}
		rgb[0] = (uint8_t)r; rgb[1] = (uint8_t)g; rgb[2] = (uint8_t)b;
		rc = cmd_tx(fd, CMD_BEGIN, args, NULL, 0, resp);
		if (!rc)
			rc = mem_write(fd, (uint16_t)(p * PROF_STRIDE + P_COLOR), rgb, 3);
		if (!rc)
			rc = cmd_tx(fd, CMD_COMMIT, args, NULL, 0, resp);
		if (!rc)
			rc = profile_activate(fd, (int)p);
		if (!rc)
			printf("perfil %lu: color = %lu,%lu,%lu\n", p, r, g, b);
		else
			rc = 1;
	} else if (!strcmp(cmd, "key")) {
		unsigned long p, r, g, b;
		int idx;

		if (argc != 7 || parse_uint(argv[2], NPROFILES - 1, &p)) {
			fprintf(stderr, "perfil debe ser 0..%d\n", NPROFILES - 1);
			rc = 2;
			goto out;
		}
		idx = key_lookup(argv[3]);
		if (idx < 0) {
			unsigned long v;
			if (parse_uint(argv[3], 200, &v) == 0)
				idx = (int)v;
		}
		if (idx < 0) {
			fprintf(stderr, "tecla desconocida: %s (use `k535ctl keys`)\n", argv[3]);
			rc = 2;
			goto out;
		}
		if (parse_uint(argv[4], 255, &r) || parse_uint(argv[5], 255, &g) ||
		    parse_uint(argv[6], 255, &b)) {
			fprintf(stderr, "valores RGB deben ser 0..255\n");
			rc = 2;
			goto out;
		}
		if (key_color_set(fd, (int)p, idx, (uint8_t)r, (uint8_t)g, (uint8_t)b) != 0) {
			rc = 1;
			goto out;
		}
		printf("perfil %lu: tecla %d (%s) = %lu,%lu,%lu\n",
		       p, idx, argv[3], r, g, b);
	} else if (!strcmp(cmd, "raw")) {
		uint8_t buf[REPORT_LEN], resp[REPORT_LEN];
		int i;
		size_t n = 0;

		if (argc < 3) {
			fprintf(stderr, "uso: %s raw <hex...>\n", argv[0]);
			rc = 2;
			goto out;
		}
		memset(buf, 0, sizeof buf);
		for (i = 2; i < argc && n < REPORT_LEN; i++) {
			char *end;
			unsigned long v = strtoul(argv[i], &end, 16);
			if (*end || v > 255) {
				fprintf(stderr, "byte invalido: %s\n", argv[i]);
				rc = 2;
				goto out;
			}
			buf[n++] = (uint8_t)v;
		}
		{
			uint16_t sum = 0;
			for (i = 3; i < REPORT_LEN; i++)
				sum += buf[i];
			buf[1] = sum & 0xff;
			buf[2] = sum >> 8;
		}
		if (hid_send(fd, buf, resp) != 0) {
			rc = 1;
			goto out;
		}
		hexdump("respuesta:", resp, REPORT_LEN);
	} else {
		usage(argv[0]);
		rc = 2;
	}

out:
	close(fd);
	return rc;
}
