Installation
============

Requirements
------------

* Rust 1.84 or newer
* GTK 4.12+ and Libadwaita 1.5+ development files
* rclone 1.68 or newer
* FUSE 3 (``fusermount3``)

On Fedora::

   sudo dnf install cargo gtk4-devel libadwaita-devel rclone fuse3

Build and install
-----------------

::

   make
   make install

This installs into ``~/.local`` by default. Set ``PREFIX`` to change that. It installs:

* ``bin/nuvolo``
* the desktop entry
* a D-Bus service file, so GNOME Files can start Nuvolo when it needs file states

Fedora packages
---------------

Every release has one RPM, for Fedora 44 and newer, attached on GitHub::

   sudo dnf install ./nuvolo-*.rpm

Uninstall
---------

::

   make uninstall
