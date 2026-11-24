#!/bin/sh
# Instalador de k535ctl + k535-gui (control RGB del teclado Redragon K535-KB).
# Requiere root para escribir en /usr/local/bin y /etc/udev/rules.d.
#
#   sudo ./install.sh              # CLI + GUI (si hay cargo)
#   sudo ./install.sh --no-gui     # solo el CLI en C
set -e

cd "$(dirname "$0")"

WITH_GUI=1
for arg in "$@"; do
	case "$arg" in
		--no-gui) WITH_GUI=0 ;;
	esac
done

if [ "$(id -u)" -ne 0 ]; then
	echo "Este instalador necesita root.  Ejecuta:"
	echo "    sudo ./install.sh"
	exit 1
fi

echo "==> Compilando k535ctl (C)"
make clean >/dev/null 2>&1 || true
make

echo "==> Instalando k535ctl"
install -Dm755 k535ctl /usr/local/bin/k535ctl
rm -f /etc/udev/rules.d/99-redragon-k535.rules
install -Dm644 udev/60-redragon-k535.rules /etc/udev/rules.d/60-redragon-k535.rules

if [ "$WITH_GUI" -eq 1 ]; then
	# localizar cargo (puede estar solo en el HOME del usuario que invoca sudo)
	REAL_HOME="$HOME"
	if [ -n "$SUDO_USER" ]; then
		REAL_HOME="$(getent passwd "$SUDO_USER" 2>/dev/null | cut -d: -f6)"
	fi
	[ -n "$REAL_HOME" ] || REAL_HOME="$HOME"
	CARGO=""
	for c in "$(command -v cargo 2>/dev/null || true)" \
		"$REAL_HOME/.cargo/bin/cargo" \
		/home/*/.cargo/bin/cargo; do
		if [ -n "$c" ] && [ -x "$c" ]; then
			CARGO="$c"
			break
		fi
	done

	if [ -n "$CARGO" ]; then
		echo "==> Compilando k535-gui (Rust + GTK4) con $CARGO"
		# rustup necesita saber donde estan el toolchain y el registro
		if [ -d "$REAL_HOME/.rustup" ]; then
			RUSTUP_HOME="$REAL_HOME/.rustup"; export RUSTUP_HOME
		fi
		if [ -d "$REAL_HOME/.cargo" ]; then
			CARGO_HOME="$REAL_HOME/.cargo"; export CARGO_HOME
		fi
		( cd rust && "$CARGO" build --release )
		install -Dm755 rust/target/release/k535-gui /usr/local/bin/k535-gui
		install -Dm644 rust/k535-gui.desktop \
			/usr/local/share/applications/k535-gui.desktop
	else
		echo "AVISO: no se encuentra cargo; se omite la GUI."
		echo "       Instala Rust (https://rustup.rs) y ejecuta de nuevo este script."
	fi
fi

echo "==> Recargando reglas udev"
udevadm control --reload-rules 2>/dev/null || true
udevadm trigger --subsystem-match=hidraw 2>/dev/null || true

echo
echo "Instalado.  Prueba:"
echo "    k535ctl list"
echo "    k535ctl dump"
echo "    k535-gui                 # interfaz grafica"
echo
echo "Si 'list' no marca la interfaz de fabricante, desenchufa y vuelve a"
echo "enchufar el teclado (o ejecuta: sudo udevadm trigger --subsystem-match=hidraw)."
