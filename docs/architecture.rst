Architecture
============

Components
----------

``rclone rcd``
   Nuvolo starts a private rclone daemon on ``127.0.0.1`` with a random password, passed
   through the environment. All work happens through its remote-control API: mounting,
   statistics, listings and links.

Engine (``src/engine.rs``)
   Owns the daemon and the sync policy. It polls ``core/stats``, ``vfs/stats`` and
   ``core/transferred`` once a second and works out the state of each file.

D-Bus service (``src/dbus.rs``)
   Exposes file states and actions to the GNOME Files extension. See :doc:`dbus`.

Window (``src/ui.rs``)
   A Libadwaita window: account sidebar, Overview, Folders, Transfers and Preferences.
   Closing the window keeps Nuvolo running, so the mounts stay up. Quit from the menu or
   with Ctrl+Q.

Mounts
------

Each account is mounted with ``--vfs-cache-mode full`` and a 5-second write-back delay.
Ignore patterns and excluded folders become ``--exclude`` rules on the mount, which is
why changing them needs a remount. Nuvolo refuses to remount while uploads are still
pending.

File states
-----------

============  =========================================================
``syncing``   rclone is transferring the file, or something inside the folder
``error``     the last transfer of this file failed
``pinned``    the file is inside a folder set to Offline
``cached``    the file has data in the VFS cache
``cloud``     none of the above: the file is only in the cloud
============  =========================================================

Known limitations
-----------------

* **Freeing cache space is not immediate.** rclone cannot evict a single file from the
  VFS cache, so *Free Local Cache* only unpins the file. rclone's LRU cleaner removes it
  once it is past the cache's maximum age, or when the cache needs the space.
* **Offline folders are kept by re-reading them.** Pinned folders are read again every
  hour. If the cache is smaller than all pinned folders together, some pinned files will
  still be evicted.
* **Pause is a bandwidth limit.** rclone has no pause command, so Pause All limits
  bandwidth to 1 byte/s.
* **Conflicts.** In VFS mode the last write wins. There is no conflict resolution yet.
* **New remotes are created with** ``rclone config``. There is no OAuth setup inside the
  app yet.
