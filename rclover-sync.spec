%global debug_package %{nil}

Name:           rclover
Version:        0.3.3
Release:        1%{?dist}
Summary:        GNOME cloud-drive manager with on-demand rclone mounts
License:        GPL-3.0-or-later
URL:            https://github.com/cyyth/rclover-sync
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  make
BuildRequires:  pkgconfig(gtk4)
BuildRequires:  pkgconfig(libadwaita-1)
Requires:       rclone
Requires:       fuse3
Recommends:     rclover-nautilus
# Formerly Nuvolo: replaces it on upgrade.
Provides:       nuvolo = %{version}-%{release}
Obsoletes:      nuvolo < 0.3.0

%description
rclover mounts Google Drive, OneDrive and any other rclone remote under ~/Cloud,
downloads files only when they are opened, and keeps chosen folders offline.

%prep
%autosetup -n rclover-sync-%{version}

%build
cargo build --release --locked

%check
cargo test --release --locked

%install
make install DESTDIR=%{buildroot} PREFIX=%{_prefix}

%files
%license LICENSE
%doc README.md
%{_bindir}/rclover
%{_datadir}/applications/io.github.cyythanapong_netizen.Rclover.desktop
%{_datadir}/dbus-1/services/io.github.cyythanapong_netizen.Rclover.service
%{_datadir}/icons/hicolor/*/apps/io.github.cyythanapong_netizen.Rclover*.svg

%changelog
* Mon Nov 23 2026 Thanapong Chuangyanyong - 0.3.3-1
- Storage on This Device is one bar: the On-demand cache (green) and the Offline folders
  (blue) fill it, and the empty part is what is left of the limit
- Softer logo background; .deb packages for Debian and Ubuntu next to the RPMs

* Mon Nov 23 2026 Thanapong Chuangyanyong - 0.3.2-1
- UI/UX improvements: active account checkmark and live mount status in the sidebar, plus
  Remove Account in Preferences
- Shared OAuth client warning can be dismissed per account
- Overview stat cards wrap error explanations, show concise session totals, and scale
  Offline and On-demand storage bars proportionally
- Folders & Cache notes when subfolders inside a folder have custom modes; Transfers & Queue
  explains failed transfer errors in plain words with direction/status icons
- Home Folders rows use semantic XDG folder icons; stepping cache settings reuses a single
  remount toast; collapsed sidebar opens the active page on single click
- Documentation uses the Shibuya Sphinx theme with the rclover logo and updated screenshots

* Sun Nov 22 2026 Thanapong Chuangyanyong - 0.3.1-1
- Upgrading from Nuvolo keeps the sync state of offline folders, so files deleted in the
  cloud are no longer uploaded again
- Flatpak apps (Steam, ...) start again after a home folder is moved to the cloud or back:
  their leftover links and empty folders in ~/.var/app are removed

* Sun Nov 22 2026 Thanapong Chuangyanyong - 0.3.0-1
- Renamed from Nuvolo to rclover (rclone + clover); settings, offline copies and sync state
  move over on first start, and the package replaces nuvolo
- Offline folders renamed or moved in the cloud stay offline under their new name; ones
  deleted in the cloud go to the Trash instead of being uploaded again
- Home Folders lists every XDG folder, can link to any folder on the drive, and keeps
  user-dirs.dirs from being reset at login (which removed the folder icons in GNOME Files)

* Sun Nov 22 2026 Thanapong Chuangyanyong - 0.2.5-1
- Offline copies sync both ways once per mount, then follow the cloud one way, so uploads
  through the mount no longer abort syncs or bring deleted files back
- Syncs wait for pending uploads; safer deletion stops for small and large folders
- Unpinning no longer copies unchanged files back as duplicates
- Notification names the programs keeping the drive busy when going offline
- rclone restarts back off instead of retrying every 5 s
- Folder listings are kept and preloaded on clouds that report changes: folders open in
  milliseconds in GNOME Files
- One RPM for Fedora 44 and newer

* Fri Nov 20 2026 Thanapong Chuangyanyong - 0.2.4-1
- Fix rclone dying immediately after startup: spawn rclone rcd from a dedicated long-lived
  background thread so PR_SET_PDEATHSIG does not fire when a short-lived worker exits

* Fri Nov 20 2026 Thanapong Chuangyanyong - 0.2.3-1
- Data safety: no-skip stray rescue copy, home-folder crash recovery for link/unlink,
  vfs/queue upload tracking with bounded retry, non-aborting stray errors, excluded-root
  stray skipping and pin validation
- Responsiveness and recovery: off-thread rclone startup/restart with port fallback,
  off-thread mount/unmount/health/options/bwlimit/config/undo, shared HTTP agent with short
  local timeouts, rclone watchdog, generation-tagged background results, canonical mount
  paths, and quoted Exec= in autostart desktop entries
- Offline availability: unmount after 15 s grace period on network drop to expose offline
  copies; timeout-bounded FUSE health probe handling EIO and hangs
- Speed: VFS/mount tuning, adaptive polling, widget state caching, single-pass conflict
  tidying and sizing, staggered periodic syncs, thin-LTO release profile, per-account cache
  budget with free-space guard, sequenced sync_now, rclone error log, and rclone >= 1.66 check
- GNOME Files: stop trashing into .Trash-<uid> inside the cloud; add GetStatesWithTypes
  D-Bus method for zero-mount emblem lookups

* Wed Nov 18 2026 Thanapong Chuangyanyong - 0.2.2-1
- Storage card with two bars: Offline folders (always kept) and the on-demand cache
- Conflicts setting (Keep Both by default, Keep the Newer, This Device Wins, Cloud Wins);
  conflict copies keep their extension: "name (conflict 1).ext"
- Offline sync errors in plain words; Sync Now and Sync Anyway buttons; stale bisync locks
  cleared; an offline copy found empty is refilled from the cloud
- Single pinned files sync with the same two-way sync and conflict handling as folders
- Help dialog (F1): what the GNOME Files icons mean, how conflicts are settled

* Wed Nov 18 2026 Thanapong Chuangyanyong - 0.2.1-1
- Home Folders only make the link: the folder's mode comes from Folders & Cache (no more
  automatic pinning or Start at Login); rows show the mode, mixed modes, and are greyed
  out for Excluded folders
- Storage in two parts: Offline folders are a fixed baseline, the cache size limit is only
  for on-demand files; pinned files are no longer also loaded into the cache
- Background Sync now syncs Offline folders every 10 minutes

* Wed Nov 18 2026 Thanapong Chuangyanyong - 0.2.0-1
- Offline copies: pinned folders and files stay available at the same path when Nuvolo is
  not running or the drive cannot be mounted, synced both ways with the cloud
- Starts at once without internet; mounting runs in the background and says
  "No internet connection", and retries as soon as the network returns
- Retry button for mounting (Overview and top-bar menu)
- Errors lead with what happened in plain words
- Source reorganised into engine/, rclone/, homedirs/ and ui/ modules

* Wed Nov 18 2026 Thanapong Chuangyanyong - 0.1.4-1
- Home folder moves are recorded until finished and uploaded: interrupted moves resume
  by themselves, failed ones offer Resume and Undo; resuming never duplicates files
- Poll rclone and answer GNOME Files off the main thread, so neither freezes
- Every error says what to do; bulk, self-replacing desktop notifications
- Moving a folder to the cloud turns on Start at Login

* Wed Nov 18 2026 Thanapong Chuangyanyong - 0.1.3-1
- Progress and duplicate-start protection for Move to Cloud, Move Back and Reconnect

* Wed Nov 18 2026 Thanapong Chuangyanyong - 0.1.2-1
- Recover from dead mounts ("Transport endpoint is not connected")
- Restart rclone if it dies; never leave a dead mount behind on quit

* Wed Nov 18 2026 Thanapong Chuangyanyong - 0.1.1-1
- Keep mounts alive: never detach busy mounts, remount lost ones
- Live data speed, parallel offline downloads, partial folder state
- Home folders in the cloud, tray icon, Start at Login, About dialog

* Wed Nov 18 2026 Thanapong Chuangyanyong - 0.1.0-1
- First package
