%global debug_package %{nil}

Name:           nuvolo
Version:        0.1.2
Release:        1%{?dist}
Summary:        GNOME cloud-drive manager with on-demand rclone mounts
License:        GPL-3.0-or-later
URL:            https://github.com/cyythanapong-netizen/nuvolo-sync
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  make
BuildRequires:  pkgconfig(gtk4)
BuildRequires:  pkgconfig(libadwaita-1)
Requires:       rclone
Requires:       fuse3
Recommends:     nuvolo-nautilus

%description
Nuvolo mounts Google Drive, OneDrive and any other rclone remote under ~/Cloud,
downloads files only when they are opened, and keeps chosen folders offline.

%prep
%autosetup -n nuvolo-sync-%{version}

%build
cargo build --release --locked

%check
cargo test --release --locked

%install
make install DESTDIR=%{buildroot} PREFIX=%{_prefix}

%files
%license LICENSE
%doc README.md
%{_bindir}/nuvolo
%{_datadir}/applications/io.github.cyythanapong_netizen.Nuvolo.desktop
%{_datadir}/dbus-1/services/io.github.cyythanapong_netizen.Nuvolo.service
%{_datadir}/icons/hicolor/*/apps/io.github.cyythanapong_netizen.Nuvolo*.svg

%changelog
* Wed Nov 18 2026 Thanapong Chuangyanyong - 0.1.2-1
- Recover from dead mounts ("Transport endpoint is not connected")
- Restart rclone if it dies; never leave a dead mount behind on quit

* Wed Nov 18 2026 Thanapong Chuangyanyong - 0.1.1-1
- Keep mounts alive: never detach busy mounts, remount lost ones
- Live data speed, parallel offline downloads, partial folder state
- Home folders in the cloud, tray icon, Start at Login, About dialog

* Wed Nov 18 2026 Thanapong Chuangyanyong - 0.1.0-1
- First package
