/// Transitional struct kept during renderer refactor. This should be removed
/// and consolidated into `Config` when implementing the full config system.
#[derive(Debug, Clone)]
pub struct SpawnConfig {
    pub initial_cols: u16,
    pub initial_rows: u16,
    pub shell_program: String,
    pub shell_args: Vec<String>,
    pub term: String,
    pub color_term: String,
}

impl Default for SpawnConfig {
    fn default() -> Self {
        Self {
            initial_cols: 80,
            initial_rows: 24,
            #[cfg(windows)]
            shell_program: "powershell.exe".into(),
            #[cfg(unix)]
            shell_program: "/bin/sh".into(),
            shell_args: Vec::new(),
            term: "xterm-256color".into(),
            color_term: "truecolor".into(),
        }
    }
}
