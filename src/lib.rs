#![doc = include_str!("../README.md")]

use std::{collections::BTreeMap, io::Write, ops::Range};

use opts::{Format, ToDump};
pub mod asm;
pub mod cached_lines;
pub mod demangle;
pub mod llvm;
pub mod mca;
pub mod mir;
pub mod opts;

#[macro_export]
macro_rules! color {
    ($item:expr, $color:expr) => {
        owo_colors::OwoColorize::if_supports_color(&$item, owo_colors::Stream::Stdout, $color)
    };
}

/// Safe version of `print[ln]!` macro
/// By default `print[ln]!` macro panics when print fails. Usually print fails when output
/// stream is disconnected, for purposes of this application disconnected stream means output
/// was piped somewhere and this something was terminated before printing completed.
///
/// At this point we might as well exit
#[macro_export]
macro_rules! safeprintln {
    ($($x:expr),* $(,)?) => {{
        use std::io::Write;
        if writeln!(std::io::stdout(), $($x),*).is_err() {
            std::process::exit(0);
        }
    }};
}

#[macro_export]
macro_rules! safeprint {
    ($($x:expr),* $(,)?) => {{
        use std::io::Write;
        if write!(std::io::stdout(), $($x),*).is_err() {
            std::process::exit(0);
        }
    }};
}

#[macro_export]
macro_rules! esafeprintln {
    ($($x:expr),* $(,)?) => {{
        use std::io::Write;
        if writeln!(std::io::stderr(), $($x),*).is_err() {
            std::process::exit(0);
        }
    }};
}

#[macro_export]
macro_rules! esafeprint {
    ($($x:expr),* $(,)?) => {{
        use std::io::Write;
        if write!(std::io::stderr(), $($x),*).is_err() {
            std::process::exit(0);
        }
    }};
}

#[derive(Debug, Clone, Ord, PartialOrd, Eq, PartialEq)]
pub struct Item {
    /// demangled name
    pub name: String,
    /// demangled name with hash
    pub hashed: String,
    /// sequential number of demangled name
    pub index: usize,
    /// number of lines
    pub len: usize,
}

pub fn suggest_name<'a>(search: &str, full: bool, items: impl IntoIterator<Item = &'a Item>) {
    let mut count = 0usize;
    let names = items.into_iter().fold(BTreeMap::new(), |mut m, item| {
        count += 1;
        m.entry(if full { &item.hashed } else { &item.name })
            .or_insert_with(Vec::new)
            .push(item.len);
        m
    });

    if names.is_empty() {
        if search.is_empty() {
            safeprintln!("This target defines no functions (or cargo-show-asm can't find them)");
        } else {
            safeprintln!("No matching functions, try relaxing your search request");
        }
        safeprintln!("You can pass --everything to see the demangled contents of a file");
    } else {
        safeprintln!("Try one of those by name or a sequence number");
    }

    #[allow(clippy::cast_sign_loss)]
    #[allow(clippy::cast_precision_loss)]
    let width = (count as f64).log10().ceil() as usize;

    let mut ix = 0;
    for (name, lens) in &names {
        safeprintln!(
            "{ix:width$} {:?} {:?}",
            color!(name, owo_colors::OwoColorize::green),
            color!(lens, owo_colors::OwoColorize::cyan),
        );
        ix += lens.len();
    }

    std::process::exit(1);
}

/// Pick an item to dump based on a goal
///
/// Prints suggestions and exits if goal can't be reached or more info is needed
#[must_use]
pub fn get_dump_range(
    goal: ToDump,
    fmt: &Format,
    items: BTreeMap<Item, Range<usize>>,
) -> Option<Range<usize>> {
    if items.len() == 1 {
        return Some(
            items
                .into_iter()
                .next()
                .expect("We just checked there's one item present")
                .1,
        );
    }

    let dump_index = |value| {
        if let Some(range) = items.values().nth(value) {
            Some(range.clone())
        } else {
            let actual = items.len();
            safeprintln!(
                "You asked to display item #{value} (zero based), but there's only {actual} items"
            );
            std::process::exit(1);
        }
    };

    match goal {
        // to dump everything just return an empty range
        ToDump::Everything => None,

        // By index without filtering
        ToDump::ByIndex { value } => dump_index(value),

        // By index with filtering
        ToDump::Function { function, nth } => {
            let filtered = items
                .iter()
                .filter(|(item, _range)| item.name.contains(&function))
                .collect::<Vec<_>>();

            let range = if nth.is_none() && filtered.len() == 1 {
                filtered
                    .get(0)
                    .expect("Must have one item as checked above")
                    .1
                    .clone()
            } else if let Some(range) = nth.and_then(|nth| filtered.get(nth)) {
                range.1.clone()
            } else if let Some(value) = nth {
                let filtered = filtered.len();
                safeprintln!("You asked to display item #{value} (zero based), but there's only {filtered} matching items");
                std::process::exit(1);
            } else {
                if filtered.is_empty() {
                    safeprintln!("Can't find any items matching {function:?}");
                } else {
                    suggest_name(&function, fmt.full_name, filtered.iter().map(|x| x.0));
                }
                std::process::exit(1);
            };
            Some(range)
        }

        ToDump::Interactive => {
            use std::process::{Command, Stdio};

            // TODO: check for various fuzzy finders in PATH
            let mut selector = Command::new("fzf");
            selector
                .arg("--no-sort")
                .arg("--tac")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped());

            let preview = true;
            if preview {
                let src_cmd: Vec<String> = std::env::args()
                    .filter(|x| x != "-i" && x != "--interactive")
                    .collect();
                let mut preview = src_cmd.join(" ");

                if fmt.color {
                    preview.push_str(" --color");
                }

                // TODO: make platform agnostic or expose --quiet
                preview.push_str(" {1} 2> /dev/null");

                selector
                    .args(["--delimiter", ": "])
                    .args(["--nth", "2"])
                    .args(["--preview-window", "up,60%,border-horizontal"])
                    .arg("--preview")
                    .arg(preview);
            }

            let selector = selector
                .spawn()
                .expect("Failed to start interactive process");

            let mut input = selector.stdin.as_ref().expect("Pipe closed unexpectedly");

            let width = items.len().ilog10() as usize + 1;
            for (ix, item) in items.keys().enumerate() {
                // TODO: write in batches
                writeln!(input, "{:width$}: {}", ix, item.name).expect("Pipe closed unexpectedly");
            }

            let out = selector
                .wait_with_output()
                .expect("Interactive Process Failure");
            // TODO: handle empty select
            let selected_index = String::from_utf8(out.stdout)
                .expect("Non valid UTF-8")
                .trim_start()
                .split_once(':')
                .and_then(|(first, _)| first.parse::<usize>().ok())
                .expect("Expected format (num: text)");

            dump_index(selected_index)
        }

        // Unspecified, so print suggestions and exit
        ToDump::Unspecified => {
            let items = items.into_keys().collect::<Vec<_>>();
            suggest_name("", fmt.full_name, &items);
            unreachable!("suggest_name exits");
        }
    }
}
