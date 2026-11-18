# Nuvolo

A GNOME app for your cloud drives. Nuvolo mounts Google Drive, OneDrive and any other
[rclone](https://rclone.org) remote under `~/Cloud`, downloads files only when you open them,
and keeps the folders you pick available offline.

It pairs with [nuvolo-nautilus](https://github.com/cyythanapong-netizen/nuvolo-nautilus),
which adds status emblems and a right-click menu to GNOME Files.

## Install

Needs Rust, GTK 4, Libadwaita 1.5+, rclone and FUSE 3.

```sh
make
make install
nuvolo
```

## Documentation

Full documentation lives in [`docs/`](docs/) (reStructuredText, built with Sphinx: `make docs`).

## License

GPL-3.0-or-later
