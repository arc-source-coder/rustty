use std::sync::Arc;

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

#[derive(Debug, Clone)]
pub struct RenderConfig {
    pub font_family: Arc<str>,
    pub font_size: f32,
    pub cursor_blink: bool,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            font_family: Arc::from("Cascadia Code"),
            font_size: 14.0,
            cursor_blink: true,
        }
    }
}
