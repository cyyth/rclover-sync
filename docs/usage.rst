Usage
=====

Adding an account
-----------------

Nuvolo mounts existing rclone remotes. Create one first::

   rclone config

Then choose **Add Cloud Account** in Nuvolo. The remote is mounted at ``~/Cloud/<remote>``.

Sync modes
----------

Each folder has one of three modes, set on the **Folders & Cache** page:

On-Demand
   The folder is listed, but files use no disk space until opened. Opened files stay in
   the VFS cache until they expire or the cache is full.

Offline
   Nuvolo reads the whole folder into the cache and re-reads it every hour, so it stays
   available without a connection.

Excluded
   The folder is hidden from the mount and never transferred. This is useful for
   ``node_modules``, build output and similar folders.

A mode applies to the folder and everything inside it, unless a subfolder sets its own.
Turning off **On-Demand Streaming** on the Overview page pins the whole drive.

Pausing
-------

**Pause All** throttles rclone's bandwidth to almost nothing. The mount stays readable
for files that are already cached.

Custom OAuth client
-------------------

Google Drive remotes created with rclone's built-in client share their API quota with every
other rclone user. Nuvolo shows a banner when that is the case. Create a *Desktop app*
OAuth client in Google Cloud Console, enter it with **Use Custom Client ID**, then use
**Reconnect Account** in Preferences.

GNOME Files integration
-----------------------

Install `nuvolo-nautilus <https://github.com/cyythanapong-netizen/nuvolo-nautilus>`_ to
get status emblems and a **Nuvolo** right-click menu in GNOME Files.
