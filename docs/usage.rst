Usage
=====

Adding an account
-----------------

rclover mounts existing rclone remotes. Create one first::

   rclone config

Then choose **Add Cloud Account** in rclover. The remote is mounted at ``~/Cloud/<remote>``.

Sync modes
----------

Each folder has one of three modes, set on the **Folders & Cache** page:

On-Demand
   The folder is listed, but files use no disk space until opened. Opened files stay in
   the VFS cache until they expire or the cache is full.

Offline
   The folder is always on this device, even when rclover is not running. See
   :ref:`offline-copies`.

Excluded
   The folder is hidden from the mount and never transferred. This is useful for
   ``node_modules``, build output and similar folders.

A mode applies to the folder and everything inside it, unless a subfolder sets its own.
Excluding a folder remounts the drive to hide it. If uploads are still pending, rclover waits
for them to finish first.
Turning off **On-Demand Streaming** on the Overview page pins the whole drive.

.. _offline-copies:

Offline copies
--------------

A folder (or a single file) set to Offline keeps a real copy in the mount folder itself,
for example ``~/Cloud/gdrive/Music``. While the drive is mounted, the mount covers that
folder and you see the live drive. When rclover is not running, or the drive cannot be
mounted (no internet), the same path shows the offline copy instead. So anything that links
into an Offline folder keeps working at all times: home folders moved to the cloud, and
files you link from ``~/.config`` (pin the file or its folder).

rclover syncs the copies with the cloud after every mount, every 10 minutes (with
**Background Sync** on), and on **Sync Now** (Overview page, or the rclover menu in GNOME
Files). Right after the drive is mounted, the copies sync both ways: what you changed while
offline is uploaded, deletions included. While the drive is mounted you work in the drive
itself, so from then on the copies just follow the cloud. Each sync waits until your recent
edits have finished uploading.

If the network drops while a terminal or another program has a folder of the drive open,
rclover cannot switch to the offline copies. It tells you which programs keep the drive busy
and tries again by itself once they let go.

.. _conflicts:

Conflicts
~~~~~~~~~

A file that changed both on this device and in the cloud since the last sync is a
conflict. Preferences → **Conflicts** sets what happens:

Keep Both (default)
   The newer version keeps the name; the other is kept next to it as
   ``name (conflict 1).ext``, so it still opens with the right app. Nothing is lost.
   (A single pinned file keeps rclone's name for the copy, ``name.ext.conflict1``.)
Keep the Newer
   The newer version is kept; the older one is deleted.
This Device Wins / Cloud Wins
   That side's version is kept; the other is deleted.

The first sync of a newly pinned folder merges both sides: the newer version of a file wins,
and nothing is deleted.

Files opened through the mounted drive are different: saving uploads them a few seconds
later, so if the same file changed in the cloud meanwhile, the last save wins.

Safety stops
~~~~~~~~~~~~

* If a sync would delete more than half of an offline folder, or every file in it changed
  at once, rclover stops and says so. If that was intended, click **Sync Anyway** on the
  Overview page.
* If every file in an offline copy was deleted while the drive was not mounted, rclover
  puts them back from the cloud. To delete files for good, delete them in the drive while
  rclover runs.

Changes made while rclover was not running are uploaded the next time it mounts the drive.
Files saved directly into ``~/Cloud/<drive>`` outside Offline folders while it was not
mounted are moved into the drive then, as are the copies of folders that are no longer
Offline (that is how unpinning frees the space).

The **Offline Copies** row on the Overview page shows whether they are up to date.

A pinned folder inside an On-Demand one works too: with rclover closed, ``~/Cloud/gdrive/A``
then holds just ``B``, the pinned folder, with all its files.

Storage on this device
~~~~~~~~~~~~~~~~~~~~~~

Local storage has two parts, each with its own bar on the Overview page:

Offline folders (blue)
   Always kept, as large as what you set to Offline. Pinning 100 GiB takes 100 GiB.

On-demand cache (green)
   Files you open from On-Demand folders, up to **On-Demand Cache Size** in Preferences.
   Files come and go: the oldest are cleared when it is full, or after the cache's maximum
   age.

So with 100 GiB pinned and a 20 GiB cache, rclover uses up to 120 GiB, of which 20 GiB
changes as you open files.

Home folders
------------

Preferences → **Home Folders** moves Documents, Pictures, Music and the other XDG folders
into the selected drive. The real folder then lives in the drive (for example
``~/Cloud/gdrive/Documents``), and ``~/Documents`` becomes a link to it. **Choose
Folder…** links it to another folder on the drive instead, which may have a different name
(for example ``~/Pictures`` → ``~/Cloud/gdrive/Photos``).

All XDG folders are listed, also ones that do not exist yet. Linking a folder keeps its
entry in ``~/.config/user-dirs.dirs`` and turns off ``xdg-user-dirs-update``
(``enabled=False`` in ``~/.config/user-dirs.conf``): at login it runs before the drive is
mounted, sees the link pointing nowhere and resets the folder to ``~``, which removes its
icon in GNOME Files. rclover also repairs such entries for linked folders when it starts.

That is all a home folder setting does: it makes the link. How the cloud folder is kept is
set on the **Folders & Cache** page like for any other folder: On-Demand, Offline, or a mix
of modes for the folders inside it. Each row shows the current mode. A folder set to
Excluded is hidden from the drive, so its row is greyed out until you change that.

Moving never deletes anything:

1. The original folder is renamed to ``~/.local/share/rclover/backups/<Name>-<date>``.
2. ``~/<Name>`` becomes a link to the cloud folder.
3. The files are copied into the cloud folder. A name that already exists there is kept
   side by side as ``name (this device).ext``.

4. rclover waits until rclone has uploaded the files. Only then is the move finished, and a
   notification says so.

While a folder is being moved, its row shows a spinner and how many files have been copied
(then how many are left to upload). It cannot be started again until it finishes. The move
runs in the background; closing the window does not stop it.

Delete the backup yourself once you have checked the cloud copy. **Move Back** downloads a
copy of the cloud folder and puts a real folder back in ``~``; the cloud copy is kept.

Unfinished moves
~~~~~~~~~~~~~~~~

A move in progress is recorded in ``~/.local/share/rclover/jobs/<Name>.json`` and the record
is removed only when the move has finished. So rclover always knows when a folder is only
partly moved, even after a crash:

* **Interrupted** (rclover quit or crashed, the drive was not mounted): it resumes by itself
  once the drive is mounted.
* **Failed** (for example a file that cannot be read): the row explains the error and
  offers **Resume** and **Undo** (**Cancel** for a move back). Undo puts the original folder
  back from its backup; Cancel deletes only the partial copy.

Resuming is safe: a file already present with the same name and size is skipped, not copied
twice, and a file is copied under a temporary name first, so a cut-off copy never looks
complete.

Moves started by rclover 0.1.3 or older left no record. On first start, 0.1.4 checks every
linked folder against its newest backup and copies whatever is missing.

When rclover is not running
~~~~~~~~~~~~~~~~~~~~~~~~~~~

What you get depends on the folder's mode. Whatever is set to Offline is there as its
offline copy (see :ref:`offline-copies`), and changes you make to it are uploaded the next
time rclover runs. On-Demand content is not there: the link leads to an empty folder until
rclover mounts the drive. Turn on **Start at Login** to keep that window short.

Start at login and the top bar
------------------------------

Turn on **Start at Login** in Preferences to mount your drives in the background when you
log in. No window opens; closing the window also keeps rclover running.

rclover shows a clover in the top bar with the current status and a menu to pause, open the
window or quit. GNOME needs the *AppIndicator and KStatusNotifierItem Support* extension
for that (``gnome-shell-extension-appindicator`` on Fedora).

Errors and notifications
------------------------

Every error rclover shows says what happened in plain words, then what to do about it: "No
internet connection", "Your sign-in has expired", "This device is out of disk space", and so
on. The original error is in the tooltip, for bug reports.

A drive that cannot be mounted shows the reason under **Mount Location** on the Overview
page. rclover retries every 10 seconds by itself, and right away when the network comes
back. To retry now, click **Retry** there, or **Retry Mounting** in the top-bar menu.

Without internet, rclover still starts at once: mounting runs in the background, and the
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
other rclone user. rclover shows a banner when that is the case. Create a *Desktop app*
OAuth client in Google Cloud Console, enter it with **Use Custom Client ID**, then use
**Reconnect Account** in Preferences.

For faster uploads of large files to Google Drive, you can also raise ``chunk_size`` on your
remote from rclone's default ``8M`` to ``32M`` or ``64M`` via ``rclone config`` (advanced
settings).

Deleting files
--------------

In an rclover drive, GNOME Files deletes files right away instead of moving them to its trash,
and asks you first. The file still goes to the cloud's own trash (for Google Drive, the Bin
on drive.google.com, for 30 days), so you can restore it from there.

That is because GNOME's trash does not work well on cloud drives: it moves each file into a
hidden ``.Trash-1000`` folder inside the drive, which is slow, and the files stay in the cloud
using space. rclover prevents it with an empty file named ``.Trash-<your user id>`` in the
drive's root; leave that file in place. If an older version left such a trash folder, rclover
renames it to **Trashed in GNOME Files**, so you can review it and delete it yourself.

Deleting a folder with many files still takes one request per file, so it can take a while.

Help
----

The help button in the window's header bar (or F1) explains the icons rclover shows on files
in GNOME Files, and how conflicts are settled.

GNOME Files integration
-----------------------

Install `rclover-nautilus <https://github.com/cyythanapong-netizen/rclover-nautilus>`_ to
get status emblems and a **rclover** right-click menu in GNOME Files.
