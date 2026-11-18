D-Bus interface
===============

Nuvolo exports one interface on its application bus name. The GNOME Files extension uses
it. This document is the contract between the two repositories.

:Bus name: ``io.github.cyythanapong_netizen.Nuvolo`` (session bus, D-Bus activatable)
:Object path: ``/io/github/cyythanapong_netizen/Nuvolo``
:Interface: ``io.github.cyythanapong_netizen.Nuvolo1``

All paths are absolute local paths inside a mount, such as
``/home/me/Cloud/gdrive/Documents/a.pdf``.

States are one of ``cloud``, ``cached``, ``partial``, ``pinned``, ``syncing`` and ``error``.
``partial`` is only used for folders: the folder is not pinned, but some of its content is
pinned or cached.
Paths outside every mount are left out of results.

Methods
-------

``GetMounts() → as``
   The mount points of all accounts.

``GetStates(as paths) → a{ss}``
   The state of each path.

``GetDirStates(s dir) → a{ss}``
   The state of every entry in ``dir``, keyed by absolute path. Clients should call this
   once per directory and cache the result.

``Pin(as paths)``
   Set the paths to Offline and start downloading them.

``Unpin(as paths)``
   Stop keeping the paths offline (see the limitations in :doc:`architecture`).

``SyncNow(as paths)``
   Re-list the paths from the cloud now. Pinned paths are re-read as well.

``GetShareLink(s path) → s``
   A public share link, created by the backend if needed.

``GetWebUrl(s path) → s``
   A URL that opens the file in the provider's web interface. Backends without a direct
   URL return a share link instead.

Errors are returned as ``io.github.cyythanapong_netizen.Nuvolo1.Error``.

Signals
-------

``StatesChanged(as paths)``
   The state of these paths, or of something below them, has changed. Clients should drop
   cached states for these paths and their parent folders.
