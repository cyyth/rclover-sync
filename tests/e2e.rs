//! End-to-end test for the `rclover` binary over a private D-Bus session bus.
//!
//! Spawns `rclover --gapplication-service` in an isolated temporary `$HOME` with a local rclone
//! remote, occupies port 5572 to exercise ephemeral port fallback, verifies `rclone rcd` stays
//! alive past startup (catching `PR_SET_PDEATHSIG` regressions), and exercises `GetMounts`,
//! `GetStatesWithTypes`, `Pin`, and `Unpin` over D-Bus.

use std::fs;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BUS: &str = "io.github.cyythanapong_netizen.Rclover";
const OBJ_PATH: &str = "/io/github/cyythanapong_netizen/Rclover";
const IFACE: &str = "io.github.cyythanapong_netizen.Rclover1";

fn have_tool(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

const DBUS_CONF: &str = r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#;

#[test]
fn e2e_service_starts_rclone_and_answers_dbus() {
    if !have_tool("dbus-run-session") || !have_tool("gdbus") || !have_tool("rclone") || !have_tool("gtk4-broadwayd") {
        eprintln!("skipping e2e test: requires dbus-run-session, gdbus, rclone, and gtk4-broadwayd");
        return;
    }

    if std::env::var_os("RCLOVER_E2E_CHILD").is_none() {
        let dbus_conf = std::env::temp_dir().join(format!("rclover-e2e-dbus-{}.conf", std::process::id()));
        fs::write(&dbus_conf, DBUS_CONF).unwrap();
        let exe = std::env::current_exe().expect("current_exe");
        let status = Command::new("dbus-run-session")
            .arg("--config-file")
            .arg(&dbus_conf)
            .arg("--")
            .arg(exe)
            .args(["--exact", "e2e_service_starts_rclone_and_answers_dbus", "--nocapture"])
            .env("RCLOVER_E2E_CHILD", "1")
            .env("GIO_USE_VFS", "local")
            .status()
            .expect("launch dbus-run-session");
        let _ = fs::remove_file(&dbus_conf);
        assert!(status.success(), "e2e child failed inside dbus-run-session: {status}");
        return;
    }

    let root = std::env::temp_dir().join(format!("rclover-e2e-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let home = root.join("home");
    let cfg_dir = home.join(".config");
    let cache_dir = home.join(".cache");
    let data_dir = home.join(".local/share");
    let mount = home.join("Cloud/localtest");
    fs::create_dir_all(cfg_dir.join("rclover")).unwrap();
    fs::create_dir_all(cfg_dir.join("rclone")).unwrap();
    fs::create_dir_all(&cache_dir).unwrap();
    fs::create_dir_all(&data_dir).unwrap();
    fs::create_dir_all(&mount).unwrap();

    fs::write(cfg_dir.join("rclone/rclone.conf"), "[localtest]\ntype = local\n").unwrap();
    fs::write(
        cfg_dir.join("rclover/config.toml"),
        format!(
            "rc_port = 5572\n\n[[accounts]]\nremote = \"localtest\"\nmount = \"{}\"\nbackground_sync = false\n\n[accounts.modes]\nDocs = \"pinned\"\nSecret = \"excluded\"\n",
            mount.display()
        ),
    )
    .unwrap();

    // Start a headless GTK4 Broadway display so adw::Application initializes without X11/Wayland.
    let display = format!(":{}", 20 + (std::process::id() % 70));
    let mut broadway = Command::new("gtk4-broadwayd")
        .arg(&display)
        .env("XDG_RUNTIME_DIR", &root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn gtk4-broadwayd");
    std::thread::sleep(Duration::from_millis(200));

    // Occupy port 5572 if available so rclover exercises ephemeral port fallback.
    let _occupied_5572 = TcpListener::bind(("127.0.0.1", 5572)).ok();

    let mut child = Command::new(env!("CARGO_BIN_EXE_rclover"))
        .arg("--gapplication-service")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &cfg_dir)
        .env("XDG_CACHE_HOME", &cache_dir)
        .env("XDG_DATA_HOME", &data_dir)
        .env("XDG_RUNTIME_DIR", &root)
        .env("GDK_BACKEND", "broadway")
        .env("BROADWAY_DISPLAY", &display)
        .env("GIO_USE_VFS", "local")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rclover --gapplication-service");

    let result = run_dbus_checks(&mount, &cache_dir, &mut child);

    let _ = child.kill();
    let output = child.wait_with_output().expect("wait rclover");
    let _ = broadway.kill();
    let _ = broadway.wait();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let _ = fs::remove_dir_all(&root);

    if let Err(msg) = result {
        panic!("e2e check failed: {msg}\n--- rclover stderr ---\n{stderr}");
    }
}

fn run_dbus_checks(mount: &Path, cache_dir: &Path, child: &mut std::process::Child) -> Result<(), String> {
    // 1. Wait up to 10 s for rclover to register on the private session bus.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mounts_out = loop {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            return Err(format!("rclover exited prematurely with {st}"));
        }
        if let Ok(out) = gdbus_call("GetMounts", &[]) {
            break out;
        }
        if Instant::now() > deadline {
            return Err("timed out waiting for rclover D-Bus service".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mount_str = mount.to_string_lossy();
    if !mounts_out.contains(mount_str.as_ref()) {
        return Err(format!("GetMounts did not include {mount_str}: {mounts_out}"));
    }

    // 2. Wait for rclone rcd to start (rclone.log is created) and verify rclover + rclone stay alive.
    let log_path = cache_dir.join("rclover/rclone.log");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !log_path.exists() {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            return Err(format!("rclover exited while starting rclone: {st}"));
        }
        if Instant::now() > deadline {
            return Err("rclone.log was not created within 10 s".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // Give any misbehaving PR_SET_PDEATHSIG or crash a moment to surface.
    std::thread::sleep(Duration::from_millis(500));
    if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
        return Err(format!("rclover exited shortly after startup: {st}"));
    }

    // 3. GetStatesWithTypes answers immediately from in-memory config/engine state.
    let docs = mount.join("Docs");
    let doc_file = docs.join("readme.md");
    let other_file = mount.join("Other/file.txt");
    let arg = format!(
        "[('{}', true), ('{}', false), ('{}', false)]",
        docs.display(),
        doc_file.display(),
        other_file.display()
    );
    let states = gdbus_call("GetStatesWithTypes", &[&arg])?;
    if !states.contains(&format!("'{}': 'pinned'", docs.display()))
        || !states.contains(&format!("'{}': 'pinned'", doc_file.display()))
        || !states.contains(&format!("'{}': 'cloud'", other_file.display()))
    {
        return Err(format!("unexpected GetStatesWithTypes reply: {states}"));
    }

    // 4. Pin and Unpin round-trip via D-Bus.
    let pin_arg = format!("['{}']", other_file.display());
    gdbus_call("Pin", &[&pin_arg])?;
    let after_pin = gdbus_call("GetStatesWithTypes", &[&format!("[('{}', false)]", other_file.display())])?;
    if !after_pin.contains(&format!("'{}': 'pinned'", other_file.display())) {
        return Err(format!("expected pinned after Pin: {after_pin}"));
    }
    gdbus_call("Unpin", &[&pin_arg])?;
    let after_unpin = gdbus_call("GetStatesWithTypes", &[&format!("[('{}', false)]", other_file.display())])?;
    if !after_unpin.contains(&format!("'{}': 'cloud'", other_file.display())) {
        return Err(format!("expected cloud after Unpin: {after_unpin}"));
    }

    Ok(())
}

fn gdbus_call(method: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new("gdbus")
        .args(["call", "--session", "--dest", BUS, "--object-path", OBJ_PATH, "--method", &format!("{IFACE}.{method}")])
        .args(args)
        .output()
        .map_err(|e| format!("gdbus {method}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}
