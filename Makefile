CC      ?= gcc
CFLAGS  ?= -O2 -Wall -Wextra -std=c99
PREFIX  ?= /usr/local

all: k535ctl

k535ctl: src/k535ctl.c
	$(CC) $(CFLAGS) -o $@ $<

install: k535ctl
	install -Dm755 k535ctl $(DESTDIR)$(PREFIX)/bin/k535ctl
	install -Dm644 udev/60-redragon-k535.rules $(DESTDIR)/etc/udev/rules.d/60-redragon-k535.rules

clean:
	rm -f k535ctl

.PHONY: all install clean
