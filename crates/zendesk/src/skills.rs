//! The agent skills (SKILL.md bundles) embedded in both binaries, and the `skills` subcommand
//! that installs them for AI coding agents.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

pub struct Skill {
    pub name: &'static str,
    pub content: &'static str,
}

pub const SKILLS: &[Skill] = &[
    Skill {
        name: "zendesk",
        content: include_str!("../../../skills/zendesk/SKILL.md"),
    },
    Skill {
        name: "zendesk-tickets",
        content: include_str!("../../../skills/zendesk-tickets/SKILL.md"),
    },
    Skill {
        name: "zendesk-help-center",
        content: include_str!("../../../skills/zendesk-help-center/SKILL.md"),
    },
    Skill {
        name: "zendesk-admin",
        content: include_str!("../../../skills/zendesk-admin/SKILL.md"),
    },
];

/// Agents that read only their own skills directory, not `.agents/skills`: (name, directory).
const OWN_DIR: &[(&str, &str)] = &[("claude", ".claude"), ("kiro", ".kiro")];

#[derive(clap::Subcommand)]
pub enum Command {
    /// Install the skills for AI coding agents.
    Install(InstallArgs),
    /// List the bundled skills.
    List,
}

#[derive(clap::Args)]
pub struct InstallArgs {
    /// Install under the current directory instead of the home directory.
    #[arg(long)]
    project: bool,
    /// Also copy the skills into this agent's own directory, even if it is not detected;
    /// repeatable.
    #[arg(long = "agent", value_name = "NAME", value_parser = ["claude", "kiro"])]
    agents: Vec<String>,
    /// Install into this directory only.
    #[arg(long, value_name = "DIR", conflicts_with_all = ["project", "agents"])]
    dir: Option<PathBuf>,
}

/// The skills directories under `base`: `.agents/skills`, which Codex, Cursor, Gemini CLI,
/// GitHub Copilot, OpenCode, Amp and Pi read, plus the own directory of each agent in
/// [`OWN_DIR`] that exists in `base` or is named in `agents`.
fn targets(base: &Path, agents: &[String]) -> Vec<PathBuf> {
    let mut dirs = vec![base.join(".agents").join("skills")];
    for (agent, dir) in OWN_DIR {
        if base.join(dir).exists() || agents.iter().any(|a| a == agent) {
            dirs.push(base.join(dir).join("skills"));
        }
    }
    dirs
}

/// Write every skill to `root/<name>/SKILL.md`, overwriting, and return the paths.
fn install(root: &Path) -> Result<Vec<PathBuf>> {
    SKILLS
        .iter()
        .map(|skill| {
            let dir = root.join(skill.name);
            std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            let file = dir.join("SKILL.md");
            std::fs::write(&file, skill.content)
                .with_context(|| format!("writing {}", file.display()))?;
            Ok(file)
        })
        .collect()
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::List => {
            for skill in SKILLS {
                println!("{}", skill.name);
            }
        }
        Command::Install(args) => {
            let roots = match args.dir {
                Some(dir) => vec![dir],
                None => {
                    let base = if args.project {
                        std::env::current_dir()?
                    } else {
                        std::env::home_dir().ok_or_else(|| anyhow!("no home directory found"))?
                    };
                    targets(&base, &args.agents)
                }
            };
            for root in roots {
                for file in install(&root)? {
                    println!("wrote {}", file.display());
                }
            }
            println!("Restart your agent so it picks the skills up.");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_skill_follows_the_agent_skills_spec() {
        for skill in SKILLS {
            let rest = skill
                .content
                .strip_prefix(&format!("---\nname: {}\n", skill.name))
                .unwrap_or_else(|| panic!("{} has no matching name", skill.name));
            let (front, _) = rest.split_once("\n---\n").expect("frontmatter is closed");
            let description: String = front
                .strip_prefix("description:")
                .unwrap_or_else(|| panic!("{} has no description", skill.name))
                .split_whitespace()
                .skip_while(|w| *w == ">")
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                !description.is_empty() && description.len() <= 1024,
                "{} description is {} characters",
                skill.name,
                description.len()
            );
            let name = skill.name;
            assert!(
                name.len() <= 64
                    && name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                    && !name.starts_with('-')
                    && !name.ends_with('-')
                    && !name.contains("--"),
                "{name} is not a valid skill name"
            );
            assert!(
                skill.content.lines().count() < 120,
                "{} is long",
                skill.name
            );
        }
    }

    #[test]
    fn every_skill_directory_is_registered() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills");
        let mut dirs: Vec<String> = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.file_name().into_string().unwrap())
            .collect();
        dirs.sort();
        let mut names: Vec<&str> = SKILLS.iter().map(|skill| skill.name).collect();
        names.sort();
        assert_eq!(dirs, names);
    }

    #[test]
    fn install_writes_every_skill_and_can_run_twice() {
        let tmp = tempfile::tempdir().unwrap();
        for _ in 0..2 {
            let written = install(tmp.path()).unwrap();
            assert_eq!(written.len(), SKILLS.len());
        }
        for skill in SKILLS {
            let file = tmp.path().join(skill.name).join("SKILL.md");
            assert_eq!(std::fs::read_to_string(file).unwrap(), skill.content);
        }
    }

    #[test]
    fn targets_add_an_agent_directory_when_it_exists_or_is_asked_for() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let agents = base.join(".agents").join("skills");
        let claude = base.join(".claude").join("skills");
        assert_eq!(targets(base, &[]), std::slice::from_ref(&agents));
        assert_eq!(
            targets(base, &["claude".into()]),
            [agents.clone(), claude.clone()]
        );
        std::fs::create_dir(base.join(".claude")).unwrap();
        assert_eq!(targets(base, &[]), [agents, claude]);
    }
}
