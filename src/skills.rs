//! Skills: named instruction snippets you can toggle on to steer the model
//! (personas, house styles, task playbooks — e.g. a "caveman" terse mode).
//!
//! A skill is just a Markdown file in `~/.config/aitui/skills/`. The file stem
//! is the skill name; the body is injected as an extra system message on every
//! request while the skill is active. To add one, drop a `.md` in that folder —
//! it shows up in the skill picker (`:skill`) automatically.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

/// One loaded skill.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    /// One-line summary (first non-empty line, heading markers stripped).
    pub desc: String,
    /// Full instruction text, injected as a system message when active.
    pub body: String,
    pub active: bool,
}

fn config_base() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        })
}

/// Where skill files live: `$XDG_CONFIG_HOME/aitui/skills/` (or `~/.config/...`).
pub fn skills_dir() -> PathBuf {
    config_base().join("aitui").join("skills")
}

/// File that remembers which skills were active (for the sticky-skills toggle).
fn active_file() -> PathBuf {
    config_base().join("aitui").join("active_skills.json")
}

/// Persist the names of the currently-active skills so they survive a restart.
pub fn save_active(skills: &[Skill]) {
    let active: Vec<&str> = skills
        .iter()
        .filter(|s| s.active)
        .map(|s| s.name.as_str())
        .collect();
    if let Some(parent) = active_file().parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(&active) {
        let _ = fs::write(active_file(), json);
    }
}

fn load_active_names() -> Vec<String> {
    fs::read_to_string(active_file())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Load all skills from disk, sorted by name, restoring the previously-active
/// set (sticky skills). Seeds a sample `caveman.md` on the first run so the
/// feature is discoverable out of the box.
pub fn load() -> Vec<Skill> {
    load_with_active(&HashSet::new())
}

/// Reload skill files while preserving in-memory active toggles. Sticky active
/// names still apply too, so a newly-created skill listed in active_skills.json
/// starts active after reload.
pub fn reload_preserving_active(existing: &[Skill]) -> Vec<Skill> {
    let active: HashSet<String> = existing
        .iter()
        .filter(|s| s.active)
        .map(|s| s.name.clone())
        .collect();
    load_with_active(&active)
}

fn load_with_active(extra_active: &HashSet<String>) -> Vec<Skill> {
    let active = load_active_names();
    let dir = skills_dir();
    if !dir.exists() {
        let _ = fs::create_dir_all(&dir);
        let _ = fs::write(dir.join("caveman.md"), CAVEMAN_SAMPLE);
    }

    let mut skills: Vec<Skill> = Vec::new();
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(String::from) else {
                continue;
            };
            let Ok(body) = fs::read_to_string(&path) else {
                continue;
            };
            let desc = body
                .lines()
                .map(|l| l.trim_start_matches('#').trim())
                .find(|l| !l.is_empty())
                .unwrap_or("")
                .to_string();
            let is_active = active.iter().any(|a| a == &name) || extra_active.contains(&name);
            skills.push(Skill {
                name,
                desc,
                body: body.trim().to_string(),
                active: is_active,
            });
        }
    }
    skills.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    skills
}

const CAVEMAN_SAMPLE: &str = "# Caveman — terse output, full technical accuracy\n\n\
Respond terse, like smart caveman. Keep all technical substance; cut only fluff.\n\n\
Drop: articles (a/an/the), filler (just/really/basically/actually), pleasantries\n\
(sure/certainly/happy to), hedging. Fragments OK. Prefer short synonyms (big not\n\
extensive, fix not implement-a-solution-for). Keep technical terms exact. Keep\n\
code blocks and error text unchanged.\n\n\
Pattern: `[thing] [action] [reason]. [next step].`\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_preserves_in_memory_active_skills() {
        let base = std::env::temp_dir().join(format!(
            "aitui_skills_reload_{}_{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let dir = base.join("aitui").join("skills");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("style.md"), "# Style\nBe direct.").unwrap();
        let old = std::env::var("XDG_CONFIG_HOME").ok();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &base) };

        let mut skills = load();
        assert_eq!(skills.len(), 1);
        skills[0].active = true;
        std::fs::write(dir.join("style.md"), "# Style\nBe very direct.").unwrap();
        let reloaded = reload_preserving_active(&skills);

        match old {
            Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
        let _ = std::fs::remove_dir_all(&base);

        assert_eq!(reloaded.len(), 1);
        assert!(reloaded[0].active);
        assert!(reloaded[0].body.contains("very direct"));
    }

    #[test]
    fn desc_is_first_nonempty_line_without_hashes() {
        let s = Skill {
            name: "x".into(),
            desc: "".into(),
            body: "# Title here\n\nbody".into(),
            active: false,
        };
        // Mirror the loader's desc extraction on the same body.
        let desc = s
            .body
            .lines()
            .map(|l| l.trim_start_matches('#').trim())
            .find(|l| !l.is_empty())
            .unwrap();
        assert_eq!(desc, "Title here");
    }
}
