use terminal::SpawnConfig;

use crate::types::ProfileId;

/// Semantic shell type — enables UI grouping, icons, and display logic
/// without parsing the shell_program string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellKind {
    /// Windows PowerShell (powershell.exe)
    PowerShell,
    /// PowerShell Core (pwsh / pwsh.exe, cross-platform)
    Pwsh,
    /// Command Prompt (cmd.exe)
    CommandPrompt,
    /// WSL distribution
    Wsl { distro: String },
    /// Unix default shell ($SHELL or /bin/sh)
    UnixShell,
    /// User-defined shell
    Custom,
}

/// A named, identifiable shell configuration.
///
/// Wraps `SpawnConfig` with semantic identity. Profiles are shared across
/// all windows and used to create new terminal sessions.
#[derive(Debug, Clone)]
pub struct Profile {
    pub id: ProfileId,
    pub name: String,
    pub shell_kind: ShellKind,
    pub spawn_config: SpawnConfig,
}

impl Profile {
    /// Create a profile with a SpawnConfig derived from the shell kind.
    pub fn new(name: String, shell_kind: ShellKind) -> Self {
        let spawn_config = spawn_config_for_kind(&shell_kind);
        Self {
            id: ProfileId::new(),
            name,
            shell_kind,
            spawn_config,
        }
    }
}

fn spawn_config_for_kind(kind: &ShellKind) -> SpawnConfig {
    match kind {
        ShellKind::PowerShell => SpawnConfig {
            shell_program: "powershell.exe".into(),
            ..Default::default()
        },
        ShellKind::Pwsh => SpawnConfig {
            #[cfg(windows)]
            shell_program: "pwsh.exe".into(),
            #[cfg(unix)]
            shell_program: "pwsh".into(),
            ..Default::default()
        },
        ShellKind::CommandPrompt => SpawnConfig {
            shell_program: "cmd.exe".into(),
            ..Default::default()
        },
        ShellKind::Wsl { distro } => SpawnConfig {
            shell_program: "wsl.exe".into(),
            shell_args: vec!["-d".into(), distro.clone()],
            ..Default::default()
        },
        ShellKind::UnixShell => {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
            SpawnConfig {
                shell_program: shell,
                ..Default::default()
            }
        }
        ShellKind::Custom => SpawnConfig::default(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_from_shell_kind() {
        let profile = Profile::new("Test".into(), ShellKind::CommandPrompt);
        assert_eq!(profile.spawn_config.shell_program, "cmd.exe");
        assert!(profile.spawn_config.shell_args.is_empty());
    }

    #[test]
    fn wsl_profile_has_distro_arg() {
        let profile = Profile::new(
            "Ubuntu (WSL)".into(),
            ShellKind::Wsl {
                distro: "Ubuntu".into(),
            },
        );
        assert_eq!(profile.spawn_config.shell_program, "wsl.exe");
        assert_eq!(
            profile.spawn_config.shell_args,
            vec![String::from("-d"), String::from("Ubuntu")]
        );
    }

    #[test]
    fn profile_ids_are_unique() {
        let a = Profile::new("A".into(), ShellKind::CommandPrompt);
        let b = Profile::new("B".into(), ShellKind::CommandPrompt);
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn shell_kind_equality() {
        assert_eq!(ShellKind::PowerShell, ShellKind::PowerShell);
        assert_ne!(ShellKind::PowerShell, ShellKind::Pwsh);
        assert_eq!(
            ShellKind::Wsl {
                distro: "Ubuntu".into()
            },
            ShellKind::Wsl {
                distro: "Ubuntu".into()
            },
        );
    }
}
