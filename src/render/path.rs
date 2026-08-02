use std::path::Path;

pub fn display_path(path: &Path) -> String {
    abbreviate_home(&path.display().to_string())
}

pub fn abbreviate_home(text: &str) -> String {
    let Some(home) = home_dir() else {
        return text.to_string();
    };
    if text == home {
        return "~".to_string();
    }
    text.strip_prefix(&home)
        .filter(|rest| rest.starts_with(std::path::MAIN_SEPARATOR))
        .map(|rest| format!("~{}", rest))
        .unwrap_or_else(|| text.to_string())
}

fn home_dir() -> Option<String> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| Path::new(&home).display().to_string())
}

#[cfg(test)]
mod tests {
    use super::abbreviate_home;

    #[test]
    fn abbreviates_home_prefix_only() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let home = std::path::Path::new(&home).display().to_string();
        assert_eq!(abbreviate_home(&home), "~");
        assert_eq!(
            abbreviate_home(&format!("{home}/Codes/aitui")),
            "~/Codes/aitui"
        );
        assert_eq!(
            abbreviate_home(&format!("{home}-other/project")),
            format!("{home}-other/project")
        );
    }
}
