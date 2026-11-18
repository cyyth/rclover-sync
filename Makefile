PREFIX ?= $(HOME)/.local
DESTDIR ?=
APP_ID := io.github.cyythanapong_netizen.Nuvolo
BINDIR := $(PREFIX)/bin
DATADIR := $(PREFIX)/share
ICONS := scalable/apps/$(APP_ID).svg 24x24/apps/$(APP_ID).svg 16x16/apps/$(APP_ID).svg symbolic/apps/$(APP_ID)-symbolic.svg

build:
	cargo build --release

test:
	cargo test

install:
	install -Dm755 target/release/nuvolo $(DESTDIR)$(BINDIR)/nuvolo
	install -Dm644 data/$(APP_ID).desktop $(DESTDIR)$(DATADIR)/applications/$(APP_ID).desktop
	for i in $(ICONS); do install -Dm644 data/icons/hicolor/$$i $(DESTDIR)$(DATADIR)/icons/hicolor/$$i; done
	install -d $(DESTDIR)$(DATADIR)/dbus-1/services
	sed 's|@bindir@|$(BINDIR)|' data/$(APP_ID).service.in > $(DESTDIR)$(DATADIR)/dbus-1/services/$(APP_ID).service

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/nuvolo $(DESTDIR)$(DATADIR)/applications/$(APP_ID).desktop \
	      $(DESTDIR)$(DATADIR)/dbus-1/services/$(APP_ID).service
	for i in $(ICONS); do rm -f $(DESTDIR)$(DATADIR)/icons/hicolor/$$i; done

docs:
	sphinx-build -W -b html docs docs/_build/html

.PHONY: build test install uninstall docs
