//! Turns raw errors (from rclone, the network or the file system) into one sentence saying
//! what happened, followed by what the user can do about it.

/// The error Nuvolo reports itself when the system says there is no network.
pub const NO_INTERNET: &str = "no internet connection";
/// The error Nuvolo reports when an offline copy was found empty and refilled from the cloud.
pub const OFFLINE_EMPTIED: &str = "the offline copy was emptied";
/// The error Nuvolo reports when the cloud did not answer a mount in time.
pub const CLOUD_SILENT: &str = "the cloud did not answer in time";

/// What happened and what to do. A recognised error is replaced by a plain headline
/// ("No internet connection."); an unknown one is kept as it is, followed by general advice.
pub fn explain(err: &str) -> String {
    let e = err.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| e.contains(w));
    let known = if has(&[OFFLINE_EMPTIED]) {
        "All files in its offline copy were deleted while the drive was not mounted. To be safe, Nuvolo put them back from the cloud. To delete files for good, delete them in the drive while Nuvolo runs."
    } else if has(&["too many deletes"]) {
        "More than half of its files would be deleted, so Nuvolo stopped to be safe. If you deleted them on purpose, click Sync Anyway."
    } else if has(&["all files were changed"]) {
        "Every file in it changed at once, so Nuvolo stopped to be safe. If you expected that (for example after restoring a backup), click Sync Anyway."
    } else if has(&["prior lock file", "lock file found"]) {
        "An earlier sync was cut off. Nuvolo clears that and tries again on the next sync."
    } else if has(&["bisync aborted", "bisync critical error"]) {
        "The offline copy and the cloud could not be compared. Nuvolo starts this folder afresh on the next sync (that only adds files, it deletes nothing)."
    } else if has(&["127.0.0.1", "localhost"]) && has(&["connection refused", "dial tcp"]) {
        // Before the network check: this is rclone itself not answering, not the internet.
        "Nuvolo lost contact with rclone. It restarts rclone by itself within a few seconds."
    } else if has(&[NO_INTERNET, "no such host", "network is unreachable", "temporary failure in name resolution", "dial tcp", "i/o timeout", "tls handshake timeout", "connection timed out", "no route to host"]) {
        "No internet connection. Nuvolo connects by itself when you are back online; folders kept offline stay available in the meantime."
    } else if has(&[CLOUD_SILENT]) {
        "The cloud did not answer. Check your internet connection; if it works, the service may be down. Nuvolo retries by itself."
    } else if has(&["invalid_grant", "token expired", "unauthenticated", "couldn't fetch token", "empty token", "401"]) {
        "Your sign-in has expired. Sign in again: Preferences → Reconnect Account."
    } else if has(&["ratelimit", "rate limit", "userratelimitexceeded", "quota", "too many requests", "403", "429"]) {
        "The cloud is limiting requests. Nuvolo retries by itself; to avoid it, lower Concurrent Transfers or set up your own OAuth client in Preferences."
    } else if has(&["no space left", "disk full", "enospc"]) {
        "This device is out of disk space. Free some space or lower On-Demand Cache Size in Preferences, or set fewer folders to Offline."
    } else if has(&["didn't find section", "couldn't find section", "not found in config", "config file not found"]) {
        "This rclone remote no longer exists. Create it again with “rclone config” in a terminal."
    } else if has(&["fusermount", "fuse device", "/dev/fuse"]) {
        "FUSE is missing. Install it with “sudo dnf install fuse3”, then restart Nuvolo."
    } else if has(&["executable file not found"]) || (has(&["no such file or directory"]) && !has(&["/"])) {
        "rclone is not installed. Install it with “sudo dnf install rclone”, then start Nuvolo again."
    } else if has(&["address already in use", "is port"]) {
        "Another program uses Nuvolo's rclone port. Quit other copies of Nuvolo or rclone rcd, or change rc_port in ~/.config/nuvolo/config.toml."
    } else if has(&["permission denied", "eacces"]) {
        return format!("{}. Nuvolo may not read or write that file. Fix its permissions (Files → Properties → Permissions), then try again.", trim(err));
    } else {
        return format!("{}. Nuvolo retries by itself. If it keeps happening, restart Nuvolo or report it from About → Report an Issue.", trim(err));
    };
    known.to_owned()
}

/// True if the error means the internet cannot be reached.
pub fn is_offline(err: &str) -> bool {
    explain(err).starts_with("No internet connection")
}

fn trim(err: &str) -> &str {
    err.trim_end_matches(['.', '\n', ' '])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_what_happened_and_what_to_do() {
        assert!(is_offline("Get \"https://www.googleapis.com/drive/v3/files\": dial tcp: lookup www.googleapis.com: no such host"));
        assert!(is_offline(NO_INTERNET));
        assert!(explain(CLOUD_SILENT).starts_with("The cloud did not answer"));
        assert!(explain("too many deletes").contains("Sync Anyway"));
        assert!(explain("prior lock file found: /x.lck").starts_with("An earlier sync was cut off"));
        assert!(!is_offline("Post \"http://127.0.0.1:5572/core/stats\": dial tcp 127.0.0.1:5572: connect: connection refused"));
        assert!(explain("oauth2: cannot fetch token: invalid_grant").contains("Reconnect Account"));
        assert!(explain("googleapi: Error 403: User Rate Limit Exceeded, userRateLimitExceeded").contains("limiting requests"));
        assert!(explain("write /home/x: no space left on device").contains("disk space"));
        assert!(explain("rclone rcd exited (exit status: 1); is port 5572 already in use?").contains("rc_port"));
        assert!(explain("/a/b: Permission denied (os error 13)").starts_with("/a/b: Permission denied (os error 13). "));
        assert!(explain("Transport endpoint is not connected").starts_with("Transport endpoint is not connected. "));
    }
}
