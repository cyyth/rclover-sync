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

* ``bin/rclover``
* the desktop entry
* a D-Bus service file, so GNOME Files can start rclover when it needs file states

Fedora packages
---------------

Every release has one RPM, for Fedora 44 and newer, attached on GitHub::

   sudo dnf install ./rclover-*.rpm

Debian and Ubuntu packages
--------------------------

Every release also has a ``.deb`` for Ubuntu 24.04 and newer::

   sudo apt install ./rclover_*.deb

Upgrading from Nuvolo
---------------------

rclover used to be called Nuvolo. The RPM replaces the ``nuvolo`` package, and ``make
install`` removes the files an install under the old name left. Quit Nuvolo before starting
rclover: on its first start rclover moves ``~/.config/nuvolo``, ``~/.cache/nuvolo`` and
``~/.local/share/nuvolo`` to ``rclover`` folders, so settings, offline copies, sync state and
unfinished home-folder moves carry over, and Start at Login stays on.

Uninstall
---------

::

   make uninstall
