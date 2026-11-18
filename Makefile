PREFIX ?= $(HOME)/.local
APP_ID := io.github.cyythanapong_netizen.Nuvolo
BINDIR := $(PREFIX)/bin
DATADIR := $(PREFIX)/share

build:
	cargo build --release

test:
	cargo test

install:
	install -Dm755 target/release/nuvolo $(BINDIR)/nuvolo
	install -Dm644 data/$(APP_ID).desktop $(DATADIR)/applications/$(APP_ID).desktop
	install -d $(DATADIR)/dbus-1/services
	sed 's|@bindir@|$(BINDIR)|' data/$(APP_ID).service.in > $(DATADIR)/dbus-1/services/$(APP_ID).service

autostart:
	install -d $(HOME)/.config/autostart
	sed 's|@bindir@|$(BINDIR)|' data/nuvolo-autostart.desktop.in > $(HOME)/.config/autostart/nuvolo.desktop

uninstall:
	rm -f $(BINDIR)/nuvolo $(DATADIR)/applications/$(APP_ID).desktop \
	      $(DATADIR)/dbus-1/services/$(APP_ID).service $(HOME)/.config/autostart/nuvolo.desktop

docs:
	sphinx-build -W -b html docs docs/_build/html

.PHONY: build test install autostart uninstall docs
