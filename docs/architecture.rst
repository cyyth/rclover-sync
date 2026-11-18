Architecture
============

Components
----------

Nuvolo is one process: GTK on the main thread, and worker threads for anything that waits
on the network or the disk. ``src/main.rs`` only wires the parts together. Each module
starts with a comment that lists its files.

``rclone rcd``
   Nuvolo starts a private rclone daemon on ``127.0.0.1`` with a random password, passed
   through the environment. All work happens through its remote-control API.

=====================  ==============================================================
``src/engine/``        The sync policy and all state. Lives on the main thread; slow
                       work runs on threads and reports back over a channel.
``  mod.rs``           The ``Engine`` struct, start and shutdown, the result channel.
``  poll.rs``          The once-a-second refresh: rclone calls on a worker thread,
                       results applied on the main thread.
``  mounts.rs``        Mounting in the background, remounting, healing lost mounts,
                       retrying when the network returns.
``  offline.rs``       Offline copies under the mount point (see below).
``  pins.rs``          Folder modes and keeping pinned files in the VFS cache.
``  states.rs``        The per-file state shown as emblems.
``src/rclone/``        Talking to rclone: ``client.rs`` (API), ``daemon.rs``
                       (process), ``mount.rs``, ``bisync.rs`` (two-way sync),
                       ``links.rs``.
``src/homedirs/``      Moving home folders: ``mod.rs`` (folder logic and job files),
                       ``copy.rs`` (never-overwrite, resumable copy), ``runner.rs``
                       (background runs and resuming).
``src/ui/``            The window, one file per page: ``overview.rs``,
                       ``folders.rs``, ``transfers.rs``, ``prefs.rs``, ``home.rs``,
                       ``accounts.rs``; shared helpers in ``widgets.rs``.
``src/dbus.rs``        The interface for GNOME Files (see :doc:`dbus`). State queries
                       list folders on a worker thread.
``src/tray.rs``        The top-bar icon (``ksni``, own thread, commands over a channel).
``src/notify.rs``      Desktop notifications, in bulk and replacing each other.
``src/errors.rs``      What an error means and what to do about it.
``src/config.rs``      ``~/.config/nuvolo/config.toml``.
=====================  ==============================================================

Mounts
------

Each account is mounted with ``--vfs-cache-mode full`` and a 5-second write-back delay.
Ignore patterns and excluded folders become ``--exclude`` rules on the mount, which is
why changing them needs a remount. While uploads are pending or the mount is busy (for
example, open in GNOME Files), Nuvolo postpones the remount and retries every 5 seconds.

A busy mount is never detached lazily: when the last program lets go of a lazily detached
mount, rclone's cleanup unmounts the mount point, which by then holds the new mount.

Mounting runs on a worker thread, so Nuvolo starts at once even without internet. When the
system reports no network, a mount fails at once with "no internet connection". For Google
Drive, OneDrive, Dropbox, Box and pCloud, Nuvolo first looks up the service's address, which
fails within seconds when the internet is down. Nuvolo waits at most 30 seconds for rclone to
mount. A mount that rclone completes later is picked up by the watchdog.

A watchdog runs every 5 seconds. It mounts again any account whose mount has disappeared
or is *dead*: still listed in ``/proc/self/mounts`` but failing with "Transport endpoint is
not connected" because the rclone serving it is gone. A dead mount is detached before
mounting; nothing serves it, so that is safe. If rclone itself exits, Nuvolo starts it again
and remounts everything.

On quit, Nuvolo detaches any mount rclone could not release because it was busy, so the
next start never finds a dead mount.

rclone runs with a parent-death signal, so it exits whenever Nuvolo does, even after a crash.

Offline copies
--------------

Pinned folders and files also exist as real files in the mount folder, underneath the mount.
Before mounting, Nuvolo opens the mount folder and keeps it open. Paths through that handle
(``/proc/<pid>/fd/<n>/...``) lead to the covered folder rather than into the mount. The
symlink ``~/.cache/nuvolo/offline/<remote>`` points there, so rclone's sync state, kept in
``~/.cache/nuvolo/bisync``, survives restarts.

After each mount and every 10 minutes, ``offline.rs`` first moves anything under the mount
that is not pinned into the drive (files written while it was not mounted, or copies that
are no longer pinned), then syncs each pinned folder with ``sync/bisync`` and each pinned
file with two ``operations/copyfile`` calls, the newer copy winning. Before moving anything,
it checks that the mount really is up (a different device), so it never copies a folder
onto itself.

File states
-----------

============  =========================================================
``syncing``   rclone is transferring the file, or something inside the folder
``error``     the last transfer of this file failed
``pinned``    the file is inside a folder set to Offline
``cached``    the file has data in the VFS cache
``partial``   a folder that is not pinned but holds pinned or cached content
``cloud``     none of the above: the file is only in the cloud
============  =========================================================

Known limitations
-----------------

* **Freeing cache space is not immediate.** rclone cannot evict a single file from the
  VFS cache, so *Free Local Cache* only unpins the file. rclone's LRU cleaner removes it
  once it is past the cache's maximum age, or when the cache needs the space.
* **Offline files are stored twice**: the offline copy, and the VFS cache the live mount
  reads from. Pinned files are read again every hour to keep them in the cache; if the cache
  is smaller than all pinned folders together, the live mount may need the network for
  some of them (the offline copy is unaffected).
* **Pause is a bandwidth limit.** rclone has no pause command, so Pause All limits
  bandwidth to 1 byte/s.
* **Conflicts.** Through the mount, the last write wins. Offline copies keep both versions
  of a file changed on both sides.
* **New remotes are created with** ``rclone config``. There is no OAuth setup inside the
  app yet.
