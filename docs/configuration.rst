Configuration
=============

Settings are stored in ``~/.config/nuvolo/config.toml``. The Preferences page edits
this file. Editing it by hand while Nuvolo is closed also works.

If the file does not parse, Nuvolo moves it to ``config.toml.bak`` and starts with defaults.

Example
-------

.. code-block:: toml

   transfers = 4
   checkers = 8
   cache_max_size_gib = 20
   cache_max_age_hours = 168
   no_modtime = true
   ignore = ["node_modules/**", "target/**", "_deps/**", "*.tmp", ".~lock.*#"]
   rc_port = 5572

   [[accounts]]
   remote = "gdrive"
   mount = "/home/me/Cloud/gdrive"
   background_sync = true

   [accounts.modes]
   Documents = "pinned"
   "Projects/web/node_modules" = "excluded"

Keys
----

``transfers``, ``checkers``
   rclone's ``--transfers`` and ``--checkers``. Applied immediately.

``cache_max_size_gib``, ``cache_max_age_hours``
   Limits for the VFS cache. Applied on the next remount.

``no_modtime``
   Passes ``--no-modtime`` to the mount, which saves API calls on large trees.

``ignore``
   rclone exclude patterns, applied to every account.

``rc_port``
   Port for the private ``rclone rcd`` instance on 127.0.0.1.

``accounts.modes``
   Folder paths relative to the remote root, each mapped to ``on-demand``, ``pinned``
   or ``excluded``. The empty key ``""`` is the remote root.

Cache data is kept in ``~/.cache/nuvolo``.
