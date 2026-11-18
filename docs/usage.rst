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
   The folder is always on this device, even when Nuvolo is not running. See
   :ref:`offline-copies`.

Excluded
   The folder is hidden from the mount and never transferred. This is useful for
   ``node_modules``, build output and similar folders.

A mode applies to the folder and everything inside it, unless a subfolder sets its own.
Excluding a folder remounts the drive to hide it. If uploads are still pending, Nuvolo waits
for them to finish first.
Turning off **On-Demand Streaming** on the Overview page pins the whole drive.

.. _offline-copies:

Offline copies
--------------

A folder (or a single file) set to Offline keeps a real copy in the mount folder itself,
for example ``~/Cloud/gdrive/Music``. While the drive is mounted, the mount covers that
folder and you see the live drive. When Nuvolo is not running, or the drive cannot be
mounted (no internet), the same path shows the offline copy instead. So anything that links
into an Offline folder keeps working at all times: home folders moved to the cloud, and
files you link from ``~/.config`` (pin the file or its folder).

Nuvolo syncs the copies both ways with the cloud after every mount, every 10 minutes, and on
**Sync Now**:

* a change on one side is copied to the other, deletions included;
* when both sides changed, the newer file wins and the other is kept next to it with a
  ``.conflict1`` suffix;
* if more than half of a folder's files would be deleted, the sync stops and tells you,
  in case something went wrong.

Changes made while Nuvolo was not running are uploaded the next time it mounts the drive.
Files saved directly into ``~/Cloud/<drive>`` outside Offline folders while it was not
mounted are moved into the drive then, as are the copies of folders that are no longer
Offline (that is how unpinning frees the space).

The **Offline Copies** row on the Overview page shows whether they are up to date.

.. note::
   Offline files take space twice: once as the offline copy, and once in rclone's cache,
   which the live mount reads from.

Home folders
------------

Preferences → **Home Folders** moves Documents, Pictures, Music and the other XDG folders
into the selected drive. The real folder then lives in the drive (for example
``~/Cloud/gdrive/Documents``), and ``~/Documents`` becomes a link to it. The folder is set
to Offline, so it is there even when Nuvolo is not running (see :ref:`offline-copies`).

Moving never deletes anything:

1. The original folder is renamed to ``~/.local/share/nuvolo/backups/<Name>-<date>``.
2. ``~/<Name>`` becomes a link to the cloud folder.
3. The files are copied into the cloud folder. A name that already exists there is kept
   side by side as ``name (this device).ext``.

4. Nuvolo waits until rclone has uploaded the files. Only then is the move finished, and a
   notification says so.

While a folder is being moved, its row shows a spinner and how many files have been copied
(then how many are left to upload). It cannot be started again until it finishes. The move
runs in the background; closing the window does not stop it.

Delete the backup yourself once you have checked the cloud copy. **Move Back** downloads a
copy of the cloud folder and puts a real folder back in ``~``; the cloud copy is kept.

Unfinished moves
~~~~~~~~~~~~~~~~

A move in progress is recorded in ``~/.local/share/nuvolo/jobs/<Name>.json`` and the record
is removed only when the move has finished. So Nuvolo always knows when a folder is only
partly moved, even after a crash:

* **Interrupted** (Nuvolo quit or crashed, the drive was not mounted): it resumes by itself
  once the drive is mounted.
* **Failed** (for example a file that cannot be read): the row explains the error and
  offers **Resume** and **Undo** (**Cancel** for a move back). Undo puts the original folder
  back from its backup; Cancel deletes only the partial copy.

Resuming is safe: a file already present with the same name and size is skipped, not copied
twice, and a file is copied under a temporary name first, so a cut-off copy never looks
complete.

Moves started by Nuvolo 0.1.3 or older left no record. On first start, 0.1.4 checks every
linked folder against its newest backup and copies whatever is missing.

When Nuvolo is not running
~~~~~~~~~~~~~~~~~~~~~~~~~~

The folder still works: ``~/Documents`` then shows its offline copy, and what you change
there is uploaded the next time Nuvolo runs. The copy is only as fresh as the last sync, so
moving a folder to the cloud also turns on **Start at Login**.

Start at login and the top bar
------------------------------

Turn on **Start at Login** in Preferences to mount your drives in the background when you
log in. No window opens; closing the window also keeps Nuvolo running.

Nuvolo shows a clover in the top bar with the current status and a menu to pause, open the
window or quit. GNOME needs the *AppIndicator and KStatusNotifierItem Support* extension
for that (``gnome-shell-extension-appindicator`` on Fedora).

Errors and notifications
------------------------

Every error Nuvolo shows says what happened in plain words, then what to do about it: "No
internet connection", "Your sign-in has expired", "This device is out of disk space", and so
on. The original error is in the tooltip, for bug reports.

A drive that cannot be mounted shows the reason under **Mount Location** on the Overview
page. Nuvolo retries every 10 seconds by itself, and right away when the network comes
back. To retry now, click **Retry** there, or **Retry Mounting** in the top-bar menu.

Without internet, Nuvolo still starts at once: mounting runs in the background, and the
drive shows as *Offline* with its offline copies in place.

Notifications come in bulk, never one per file. Failed transfers are summed up at most
once a minute (for example "100 sync errors", with the latest reason), and each kind of
notification replaces its previous one instead of piling up. Clicking one opens the page
with the details.

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
