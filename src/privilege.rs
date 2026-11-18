//! Maximum available privilege: root, noninteractive sudo, or explicit user fallback.
use serde_json::{Value, json};
use std::sync::OnceLock;
use tokio::process::Command;

pub fn uid() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(u32::MAX)
}
static SUDO: OnceLock<bool> = OnceLock::new();
pub async fn root_available() -> bool {
    if uid() == 0 {
        return true;
    }
    if let Some(available) = SUDO.get() {
        return *available;
    }
    let available = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        Command::new("sudo")
            .args(["-n", "--", "/usr/bin/id", "-u"])
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .is_some_and(|o| o.status.success() && o.stdout == b"0\n");
    let _ = SUDO.set(available);
    available
}
pub async fn status() -> Value {
    let root = root_available().await;
    json!({"uid":uid(),"mode":if uid()==0{"root"}else if root{"sudo"}else{"user"},"root_available":root,"policy":"maximum_available","message":if root{"Root execution channel available; graphical applications use a separate controlled display"}else{"Root channel is not established: launch with root or configure system sudo authorization. No silent claim of root privileges."}})
}
pub async fn command(program: &str) -> Command {
    if uid() != 0 && root_available().await {
        let mut command = Command::new("sudo");
        command.args(["-n", "--", program]);
        command
    } else {
        Command::new(program)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn maximum_available_channel_reports_real_effective_uid() {
        let available = root_available().await;
        let output = command("/usr/bin/id")
            .await
            .arg("-u")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .output()
            .await
            .unwrap();
        assert!(output.status.success());
        let effective = String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        assert_eq!(effective, if available { 0 } else { uid() });
    }
}
