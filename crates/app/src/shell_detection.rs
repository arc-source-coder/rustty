use crate::profile::{Profile, ShellKind};
use crate::types::ProfileId;

/// Detect available shells and build the default profile list.
///
/// On Windows: always includes Command Prompt + Windows PowerShell,
/// conditionally includes pwsh (PowerShell Core) and WSL distros.
/// On Unix: includes $SHELL (or /bin/sh fallback).
pub fn detect_profiles() -> (Vec<Profile>, ProfileId) {
    let profiles = build_platform_profiles();
    let default_id = select_default(&profiles);
    (profiles, default_id)
}

/// Select the default profile. Prefers pwsh > PowerShell > first profile.
fn select_default(profiles: &[Profile]) -> ProfileId {
    // Prefer pwsh (PowerShell Core) if available
    if let Some(p) = profiles
        .iter()
        .find(|p| matches!(p.shell_kind, ShellKind::Pwsh))
    {
        return p.id;
    }
    // Fall back to Windows PowerShell
    if let Some(p) = profiles
        .iter()
        .find(|p| matches!(p.shell_kind, ShellKind::PowerShell))
    {
        return p.id;
    }
    // Last resort: first profile
    profiles[0].id
}

#[cfg(windows)]
fn build_platform_profiles() -> Vec<Profile> {
    let mut profiles = Vec::new();

    // Always available on Windows
    profiles.push(Profile::new(
        "Command Prompt".into(),
        ShellKind::CommandPrompt,
    ));
    profiles.push(Profile::new(
        "Windows PowerShell".into(),
        ShellKind::PowerShell,
    ));

    // PowerShell Core (cross-platform) — check PATH
    if is_program_in_path("pwsh.exe") {
        profiles.push(Profile::new("PowerShell".into(), ShellKind::Pwsh));
    }

    // WSL distros
    for distro_name in detect_wsl_distros() {
        let display = format!("{} (WSL)", distro_name);
        profiles.push(Profile::new(
            display,
            ShellKind::Wsl {
                distro: distro_name,
            },
        ));
    }

    profiles
}

#[cfg(unix)]
fn build_platform_profiles() -> Vec<Profile> {
    let mut profiles = vec![Profile::new("Default Shell".into(), ShellKind::UnixShell)];

    // pwsh is cross-platform
    if is_program_in_path("pwsh") {
        profiles.push(Profile::new("PowerShell".into(), ShellKind::Pwsh));
    }

    profiles
}

// --- PATH detection ---

fn is_program_in_path(name: &str) -> bool {
    #[cfg(windows)]
    {
        std::process::Command::new("where.exe")
            .arg(name)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(unix)]
    {
        std::process::Command::new("which")
            .arg(name)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

// --- WSL detection (Windows only) ---

/// Detect installed WSL distributions by running `wsl.exe -l -q`.
///
/// Uses `-q` (quiet) mode which outputs one distro name per line with
/// no header, no `*` default marker, and no STATE/VERSION columns.
/// This avoids header parsing, localization issues (non-English headers),
/// and handles distro names with spaces correctly.
///
/// `wsl.exe` outputs UTF-16LE with BOM. We decode the raw bytes manually
/// because `std::process::Command` assumes UTF-8 stdout.
///
/// Returns an empty Vec on any failure (WSL not installed, no distros,
/// parse errors). Never panics.
#[cfg(windows)]
fn detect_wsl_distros() -> Vec<String> {
    let output = match std::process::Command::new("wsl.exe")
        .args(["-l", "-q"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    let decoded = match decode_utf16le(&output.stdout) {
        Some(s) => s,
        None => return Vec::new(),
    };

    parse_wsl_quiet_output(&decoded)
}

/// Decode UTF-16LE bytes (with optional BOM) to a Rust String.
#[cfg(windows)]
fn decode_utf16le(bytes: &[u8]) -> Option<String> {
    // Skip BOM if present (FF FE)
    let bytes = if bytes.starts_with(&[0xFF, 0xFE]) {
        &bytes[2..]
    } else {
        bytes
    };

    // Drop trailing odd byte if present (malformed output)
    let len = bytes.len() & !1;
    let bytes = &bytes[..len];

    if bytes.is_empty() {
        return None;
    }

    let u16s: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();

    Some(String::from_utf16_lossy(&u16s))
}

/// Parse the decoded `wsl -l -q` output into distro names.
///
/// Expected format (one name per line, no header):
/// Ubuntu
/// Debian
/// docker-desktop
///
/// Handles names with spaces (e.g. "Ubuntu 22.04 LTS").
/// Strips embedded NUL characters that sometimes appear in WSL output.
#[cfg(windows)]
fn parse_wsl_quiet_output(output: &str) -> Vec<String> {
    output
        .lines()
        .map(|line| line.trim().trim_matches('\0').trim())
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_profiles_returns_nonempty() {
        let (profiles, default_id) = detect_profiles();
        assert!(!profiles.is_empty());
        assert!(profiles.iter().any(|p| p.id == default_id));
    }

    #[test]
    fn select_default_prefers_pwsh() {
        let profiles = vec![
            Profile::new("cmd".into(), ShellKind::CommandPrompt),
            Profile::new("PS".into(), ShellKind::PowerShell),
            Profile::new("pwsh".into(), ShellKind::Pwsh),
        ];
        let default = select_default(&profiles);
        let selected = profiles.iter().find(|p| p.id == default).unwrap();
        assert!(matches!(selected.shell_kind, ShellKind::Pwsh));
    }

    #[test]
    fn select_default_falls_back_to_powershell() {
        let profiles = vec![
            Profile::new("cmd".into(), ShellKind::CommandPrompt),
            Profile::new("PS".into(), ShellKind::PowerShell),
        ];
        let default = select_default(&profiles);
        let selected = profiles.iter().find(|p| p.id == default).unwrap();
        assert!(matches!(selected.shell_kind, ShellKind::PowerShell));
    }

    #[test]
    fn select_default_falls_back_to_first() {
        let profiles = vec![Profile::new("shell".into(), ShellKind::UnixShell)];
        let default = select_default(&profiles);
        assert_eq!(default, profiles[0].id);
    }

    // --- WSL / UTF-16LE parsing tests (always compiled, no wsl.exe needed) ---

    #[cfg(windows)]
    #[test]
    fn decode_utf16le_with_bom() {
        // "Hi" in UTF-16LE with BOM
        let bytes = [0xFF, 0xFE, 0x48, 0x00, 0x69, 0x00];
        assert_eq!(decode_utf16le(&bytes).unwrap(), "Hi");
    }

    #[cfg(windows)]
    #[test]
    fn decode_utf16le_without_bom() {
        let bytes = [0x48, 0x00, 0x69, 0x00];
        assert_eq!(decode_utf16le(&bytes).unwrap(), "Hi");
    }

    #[cfg(windows)]
    #[test]
    fn decode_utf16le_odd_byte_dropped() {
        // Trailing odd byte is dropped (lenient)
        let bytes = [0x48, 0x00, 0x69, 0x00, 0xFF];
        assert_eq!(decode_utf16le(&bytes).unwrap(), "Hi");
    }

    #[cfg(windows)]
    #[test]
    fn parse_wsl_quiet_typical() {
        let output = "Ubuntu\nDebian\ndocker-desktop\n";
        let names = parse_wsl_quiet_output(output);
        assert_eq!(names, vec!["Ubuntu", "Debian", "docker-desktop"]);
    }

    #[cfg(windows)]
    #[test]
    fn parse_wsl_quiet_empty() {
        let output = "\n";
        let names = parse_wsl_quiet_output(output);
        assert!(names.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn parse_wsl_quiet_with_spaces_and_nul() {
        // Distro names with spaces and embedded NUL chars
        let output = "Ubuntu 22.04 LTS\n\0Debian\0\n";
        let names = parse_wsl_quiet_output(output);
        assert_eq!(names, vec!["Ubuntu 22.04 LTS", "Debian"]);
    }

    #[cfg(windows)]
    #[test]
    fn parse_wsl_quiet_crlf() {
        let output = "Ubuntu\r\nDebian\r\n";
        let names = parse_wsl_quiet_output(output);
        assert_eq!(names, vec!["Ubuntu", "Debian"]);
    }
}
