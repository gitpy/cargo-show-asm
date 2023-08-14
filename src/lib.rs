#![doc = include_str!("../README.md")]

use std::{
    collections::BTreeMap,
    io::{self, Write},
    ops::Range,
};

use opts::{Format, ToDump};

pub mod asm;
pub mod cached_lines;
pub mod demangle;

#[cfg(feature = "ipc")]
pub mod ipc;
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

    match goal {
        // to dump everything just return an empty range
        ToDump::Everything => None,

        // By index without filtering
        ToDump::ByIndex { value } => {
            if let Some(range) = items.values().nth(value) {
                Some(range.clone())
            } else {
                let actual = items.len();
                safeprintln!(
                "You asked to display item #{value} (zero based), but there's only {actual} items"
            );
                std::process::exit(1);
            }
        }

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
            panic!("Interactive Mode should already be checked")
        }

        // Unspecified, so print suggestions and exit
        ToDump::Unspecified => {
            let items = items.into_keys().collect::<Vec<_>>();
            suggest_name("", fmt.full_name, &items);
            unreachable!("suggest_name exits");
        }
    }
}

pub trait DumpRange {
    fn dump_range(&self, range: Option<Range<usize>>) -> anyhow::Result<()> {
        let mut writer = io::stdout();
        if self.dump_range_into_writer(range, &mut writer).is_err() || writer.flush().is_err() {
            std::process::exit(0); // Exit when stdout is closed
        }
        Ok(())
    }

    fn dump_range_into_writer(
        &self,
        range: Option<Range<usize>>,
        writer: &mut impl Write,
    ) -> anyhow::Result<()>;
}

pub fn interactive_mode(
    items: &BTreeMap<Item, Range<usize>>,
    dump_ctx: impl DumpRange + Send + Sync,
) {
    use std::process::{Command, Stdio};

    let delimiter = ": ";

    // TODO: check for various fuzzy finders in PATH
    let mut selector = Command::new("fzf");
    selector
        .arg("--no-sort")
        .arg("--tac")
        .args(["--delimiter", delimiter])
        .args(["--nth", "2"]) // Only fuzzy search function name
        //.args(["--with-nth", "2"]) // Only display function name
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());

    #[cfg(feature = "ipc")]
    {
        // TODO: evaluate if env::current_exe() is better
        let mut preview_arg: String = std::env::args()
            .next()
            .expect("Should only fail when the executable is unlinked");
        preview_arg.push_str(" --client --server-name=\"");
        preview_arg.push_str(&ipc::get_address()); // TODO: might require shell escape
        preview_arg.push_str("\" --select {1}");

        // TODO: maybe check terminal dimensions for smart preview layout
        selector
            .args(["--preview-window", "up,60%,border-horizontal"])
            .arg("--preview")
            .arg(preview_arg);
    }

    let selector = selector
        .spawn()
        .expect("Failed to start interactive process");

    let mut input = selector.stdin.as_ref().expect("Pipe closed unexpectedly");

    let width = items.len().ilog10() as usize + 1;
    for (ix, item) in items.keys().enumerate() {
        // TODO: write in batches
        writeln!(input, "{ix:width$}{delimiter}{}", item.name).expect("Pipe closed unexpectedly");
    }

    let wait_selector = || {
        selector
            .wait_with_output()
            .expect("Interactive Process Failure")
    };

    #[cfg(feature = "ipc")]
    let selector_out = std::thread::scope(|s| {
        s.spawn(|| {
            ipc::start_server(&items, &dump_ctx);
        });
        let output = wait_selector();

        ipc::send_server_stop();
        output
    });

    #[cfg(not(feature = "ipc"))]
    let selector_out = wait_selector();

    if !selector_out.status.success() {
        // TODO: maybe better error reporting
        esafeprintln!("Interactive process failed");
        std::process::exit(1);
    }

    let selected_index = String::from_utf8(selector_out.stdout)
        .expect("Non valid UTF-8")
        .trim_start()
        .split_once(delimiter)
        .and_then(|(first, _)| first.parse::<usize>().ok())
        .expect("Expected format (num: text)");

    let range = items
        .values()
        .nth(selected_index)
        .or_else(|| {
            esafeprintln!("Invalid index selected");
            std::process::exit(1);
        })
        .cloned();

    dump_ctx
        .dump_range(range)
        .expect("Should not fail without corruption");
}
