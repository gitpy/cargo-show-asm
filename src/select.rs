use std::{
    collections::BTreeMap,
    io::Write,
    process::{Command, Stdio},
    str,
};

use anyhow::{Context, Result};

#[cfg(feature = "ipc")]
use crate::ipc;
use crate::Item;

/// The delimiter between index and function name in finders
const DELIMITER: &str = ": ";

pub struct SelectProcess<'a> {
    pub cmd: Command,
    finder: Finder<'a>,
}

pub enum Finder<'a> {
    Fzf,
    Skim,
    Fzy,
    #[allow(dead_code)]
    Custom {
        command: &'a [&'a str],
        preview: &'a [&'a str],
    },
}

impl Finder<'_> {
    /// Scans *PATH* for fuzzy finders
    /// and returns a single opionated available finder
    pub fn in_path_suggestion() -> Option<Self> {
        use std::env;
        // In order of priority (Variant, found)
        let mut executables = [
            (Finder::Fzf, false),
            (Finder::Skim, false),
            (Finder::Fzy, false),
        ];

        if let Some(paths) = env::var_os("PATH") {
            env::split_paths(&paths).for_each(|dir| {
                for (finder, found) in executables.as_mut() {
                    let full_path = dir.join(finder.get_executable());
                    if full_path.is_file() {
                        *found = true;
                    }
                }
            });
        };

        for (finder, found) in executables {
            if found {
                return Some(finder);
            }
        }

        None
    }

    fn get_executable(&self) -> &str {
        match self {
            Finder::Fzf => "fzf",
            Finder::Skim => "sk",
            Finder::Fzy => "fzy",
            Finder::Custom { command, .. } => command[0],
        }
    }
}

impl SelectProcess<'_> {
    pub fn default_command(finder: Finder) -> SelectProcess {
        let mut cmd = Command::new(finder.get_executable());
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped());

        match finder {
            Finder::Fzf | Finder::Skim => {
                cmd.arg("--no-sort")
                    .arg("--tac")
                    .args(["--delimiter", DELIMITER])
                    .args(["--nth", "2"]) // Only fuzzy search function name
                    .args(["--with-nth", "2"]); // Only display function name
            }
            Finder::Fzy => {}
            Finder::Custom { command, .. } => {
                if command.len() > 1 {
                    cmd.args(&command[1..]);
                }
            }
        };

        let mut selector = SelectProcess { cmd, finder };
        if selector.has_preview_support() {
            selector.add_preview();
        }

        selector
    }

    fn has_preview_support(&self) -> bool {
        cfg!(feature = "ipc")
            && match self.finder {
                Finder::Fzf | Finder::Skim => true,
                Finder::Fzy => false,
                Finder::Custom { preview, .. } => !preview.is_empty(),
            }
    }

    fn add_preview(&mut self) {
        #[cfg(feature = "ipc")]
        {
            match self.finder {
                Finder::Fzf | Finder::Skim => {
                    let mut preview_cmd: String = std::env::args()
                        .next()
                        .expect("arg0 should always be the executable itself");
                    preview_cmd.push_str(" --client --server-name=\"");
                    preview_cmd.push_str(&ipc::get_address());
                    preview_cmd.push_str("\" --select {1}");

                    self.cmd
                        .args(["--preview-window", "up:60%:border-horizontal"])
                        .arg("--preview")
                        .arg(preview_cmd);
                }
                Finder::Custom { preview, .. } => {
                    for &arg in preview {
                        if arg == "PREVIEWSERVER" {
                            self.cmd.arg(&ipc::get_address());
                        } else {
                            self.cmd.arg(arg);
                        }
                    }
                }
                Finder::Fzy => {
                    unreachable!("No preview support");
                }
            }
        }
    }
}

pub fn serialize(
    writer: &mut impl Write,
    items: &BTreeMap<Item, std::ops::Range<usize>>,
) -> anyhow::Result<()> {
    let width = items.len().ilog10() as usize + 1;
    for (index, item) in items.keys().enumerate() {
        writeln!(writer, "{index:width$}{DELIMITER}{}", item.name)?;
    }
    writer.flush()?;
    Ok(())
}

pub fn deserialize(buffer: &[u8]) -> Result<usize> {
    str::from_utf8(buffer)?
        .trim_start()
        .split_once(DELIMITER)
        .context("Failed to find split")?
        .0
        .parse::<usize>()
        .map_err(anyhow::Error::msg)
}

#[test]
fn test_finder_format() {
    let item = Item {
        name: ":20pefhn4gt0ph/üde".to_string(),
        hashed: ":20pefhn4gt0ph/üde".to_string(),
        len: 0,
        index: 0,
    };
    let mut items = BTreeMap::new();
    items.insert(item, 0..0);

    let mut writer = std::io::BufWriter::new(Vec::new());
    serialize(&mut writer, &items).unwrap();

    // In this test only the first item gets checked
    let index = deserialize(&writer.into_inner().unwrap()).unwrap();
    assert_eq!(index, 0);
}
