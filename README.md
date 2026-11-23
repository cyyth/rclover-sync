<p align="center"><img src="docs/_static/logo.svg" alt="" width="96"></p>

# rclover

A GNOME app for your cloud drives. rclover mounts Google Drive, OneDrive and any other
[rclone](https://rclone.org) remote under `~/Cloud`, downloads files only when you open them,
and keeps the folders you pick available offline.

It pairs with [rclover-nautilus](https://github.com/cyyth/rclover-nautilus),
which adds status emblems and a right-click menu to GNOME Files.

## Install

Needs Rust, GTK 4, Libadwaita 1.5+, rclone and FUSE 3.

```sh
make
make install
rclover
```

## Documentation

Documentation: https://cyyth.github.io/rclover-sync/ (sources in [`docs/`](docs/), built with Sphinx).

## License

GPL-3.0-or-later
