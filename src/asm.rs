#![allow(clippy::missing_errors_doc)]
use crate::asm::statements::Label;
use crate::cached_lines::CachedLines;
use crate::demangle::LabelKind;
use crate::{
    color, demangle, esafeprintln, get_dump_range, interactive_mode, safeprintln, DumpRange, Item,
};
// TODO, use https://sourceware.org/binutils/docs/as/index.html
use crate::opts::{Format, ToDump};

mod statements;

use owo_colors::OwoColorize;
use statements::{parse_statement, Directive, Loc, Statement};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};

pub fn parse_file(input: &str) -> anyhow::Result<Vec<Statement>> {
    // eat all statements until the eof, so we can report the proper errors on failed parse
    match nom::multi::many0(parse_statement)(input) {
        Ok(("", stmts)) => Ok(stmts),
        Ok((leftovers, _)) =>
        {
            #[allow(clippy::redundant_else)]
            if leftovers.len() < 1000 {
                anyhow::bail!("Didn't consume everything, leftovers: {leftovers:?}")
            } else {
                let head = &leftovers[..leftovers
                    .char_indices()
                    .nth(200)
                    .expect("Shouldn't have that much unicode here...")
                    .0];
                anyhow::bail!("Didn't consume everything, leftovers prefix: {head:?}");
            }
        }
        Err(err) => anyhow::bail!("Couldn't parse the .s file: {err}"),
    }
}

#[must_use]
pub fn find_items(lines: &[Statement]) -> BTreeMap<Item, Range<usize>> {
    let mut res = BTreeMap::new();

    let mut sec_start = 0;
    let mut item: Option<Item> = None;
    let mut names = BTreeMap::new();

    for (ix, line) in lines.iter().enumerate() {
        #[allow(clippy::if_same_then_else)]
        if line.is_section_start() {
            if item.is_none() {
                sec_start = ix;
            } else {
                // on Windows, when panic unwinding is enabled, the compiler can
                // produce multiple blocks of exception-handling code for a
                // function, annotated by .seh_* directives (which we ignore).
                // For some reason (maybe a bug? or maybe we're misunderstanding
                // something?), each of those blocks starts with a .section
                // directive identical to the one at the start of the function.
                // We have to ignore such duplicates here, otherwise we'd output
                // only the last exception-handling block instead of the whole
                // function.
                //
                // See https://github.com/pacak/cargo-show-asm/issues/110
            }
        } else if line.is_global() && sec_start + 3 < ix {
            // on Linux and Windows every global function gets it's own section
            // on Mac for some reason this is not the case so we have to look for
            // global variables. This little hack allows to include full section
            // on Windows/Linux but still capture full function body on Mac
            sec_start = ix;
        } else if line.is_end_of_fn() {
            let sec_end = ix;
            let range = sec_start..sec_end;
            if let Some(mut item) = item.take() {
                item.len = ix - item.len;
                res.insert(item, range);
            }
        } else if let Statement::Label(label) = line {
            if let Some(dem) = demangle::demangled(label.id) {
                let hashed = format!("{dem:?}");
                let name = format!("{dem:#?}");
                let name_entry = names.entry(name.clone()).or_insert(0);
                item = Some(Item {
                    name,
                    hashed,
                    index: *name_entry,
                    len: ix,
                });
                *name_entry += 1;
            } else if label.kind == LabelKind::Unknown {
                if let Some(Statement::Directive(Directive::SectionStart(ss))) =
                    lines.get(sec_start)
                {
                    if let Some(ss) = ss.strip_prefix(".text.") {
                        if ss.starts_with(label.id) {
                            let name = label.id.to_string();
                            let name_entry = names.entry(name.clone()).or_insert(0);
                            item = Some(Item {
                                name: name.clone(),
                                hashed: name.clone(),
                                index: *name_entry,
                                len: ix,
                            });
                            *name_entry += 1;
                        }
                    }
                }
            }
        }
    }
    res
}

fn used_labels<'a>(stmts: &'_ [Statement<'a>]) -> BTreeSet<&'a str> {
    stmts
        .iter()
        .filter_map(|stmt| match stmt {
            Statement::Label(_) | Statement::Nothing => None,
            Statement::Directive(dir) => match dir {
                Directive::File(_)
                | Directive::Loc(_)
                | Directive::SubsectionsViaSym
                | Directive::Set(_) => None,
                Directive::Generic(g) => Some(g.0),
                Directive::SectionStart(ss) => Some(*ss),
            },
            Statement::Instruction(i) => i.args,
            Statement::Dunno(s) => Some(s),
        })
        .flat_map(crate::demangle::local_labels)
        .map(|m| m.as_str())
        .collect::<BTreeSet<_>>()
}

struct AsmDumpCtx<'a> {
    files: &'a BTreeMap<u64, (std::borrow::Cow<'a, Path>, Option<CachedLines>)>,
    fmt: &'a Format,
    stmts: &'a [Statement<'a>],
}

impl DumpRange for AsmDumpCtx<'_> {
    fn dump_range_into_writer(
        &self,
        range: Option<Range<usize>>,
        writer: &mut impl Write,
    ) -> anyhow::Result<()> {
        let &Self { files, fmt, stmts } = self;
        let stmts = range.map_or(stmts, |r| &stmts[r]);

        let mut prev_loc = Loc::default();

        let used = if fmt.keep_labels {
            BTreeSet::new()
        } else {
            used_labels(stmts)
        };

        let mut empty_line = false;
        for line in stmts.iter() {
            if fmt.verbosity > 2 {
                writeln!(writer, "{line:?}")?;
            }
            if let Statement::Directive(Directive::File(_)) = &line {
            } else if let Statement::Directive(Directive::Loc(loc)) = &line {
                if !fmt.rust {
                    continue;
                }
                if loc.line == 0 {
                    continue;
                }
                if loc == &prev_loc {
                    continue;
                }
                prev_loc = *loc;
                match files.get(&loc.file) {
                    Some((fname, Some(file))) => {
                        let rust_line = &file[loc.line as usize - 1];
                        let pos = format!("\t\t// {} : {}", fname.display(), loc.line);
                        writeln!(writer, "{}", color!(pos, OwoColorize::cyan))?;
                        writeln!(
                            writer,
                            "\t\t{}",
                            color!(rust_line.trim_start(), OwoColorize::bright_red)
                        )?;
                    }
                    Some((fname, None)) => {
                        if fmt.verbosity > 0 {
                            writeln!(
                                writer,
                                "\t\t{} {}",
                                color!("//", OwoColorize::cyan),
                                color!(
                            "Can't locate the file, please open a ticket with cargo-show-asm",
                            OwoColorize::red
                        ),
                            )?;
                        }
                        let pos = format!("\t\t// {} : {}", fname.display(), loc.line);
                        writeln!(writer, "{}", color!(pos, OwoColorize::cyan))?;
                    }
                    None => {
                        panic!("DWARF file refers to an undefined location {loc:?}");
                    }
                }
                empty_line = false;
            } else if let Statement::Label(Label {
                kind: kind @ (LabelKind::Local | LabelKind::Temp),
                id,
            }) = line
            {
                if fmt.keep_labels || used.contains(id) {
                    writeln!(writer, "{line}")?;
                } else if !empty_line && *kind != LabelKind::Temp {
                    writeln!(writer)?;
                    empty_line = true;
                }
            } else {
                if fmt.simplify && matches!(line, Statement::Directive(_) | Statement::Dunno(_)) {
                    continue;
                }

                empty_line = false;
                #[allow(clippy::match_bool)]
                match fmt.full_name {
                    true => writeln!(writer, "{line:#}")?,
                    false => writeln!(writer, "{line}")?,
                }
            }
        }
        Ok(())
    }
}

// DWARF information contains references to souce files
// It can point to 3 different items:
// 1. a real file, cargo-show-asm can just read it
// 2. a file from rustlib, sources are under $sysroot/lib/rustlib/src/rust/$suffix
//    Some examples:
//        /rustc/a55dd71d5fb0ec5a6a3a9e8c27b2127ba491ce52/library/core/src/iter/range.rs
//        /private/tmp/rust-20230325-7327-rbrpyq/rustc-1.68.1-src/library/core/src/option.rs
// 3. a file from prebuilt (?) hashbrown, sources are probably available under
//    cargo registry, most likely under ~/.cargo/registry/$suffix
//    Some examples:
//        /cargo/registry/src/github.com-1ecc6299db9ec823/hashbrown-0.12.3/src/raw/bitmask.rs
//        /Users/runner/.cargo/registry/src/github.com-1ecc6299db9ec823/hashbrown-0.12.3/src/map.rs
fn locate_sources(sysroot: &Path, path: &Path) -> Option<PathBuf> {
    // a real file that simply exists
    if path.exists() {
        return Some(path.into());
    }

    let no_rust_src = || {
        esafeprintln!(
            "You need to install rustc sources to be able to see the rust annotations, try\n\
                                       \trustup component add rust-src"
        );
        std::process::exit(1);
    };

    // rust sources, Linux style
    if path.starts_with("/rustc/") {
        let mut source = sysroot.join("lib/rustlib/src/rust");
        for part in path.components().skip(3) {
            source.push(part);
        }
        if source.exists() {
            return Some(source);
        } else {
            no_rust_src();
        }
    }

    // rust sources, MacOS style
    if path.starts_with("/private/tmp") && path.components().any(|c| c.as_os_str() == "library") {
        let mut source = sysroot.join("lib/rustlib/src/rust");
        for part in path.components().skip(5) {
            source.push(part);
        }
        if source.exists() {
            return Some(source);
        } else {
            no_rust_src();
        }
    }

    // cargo registry, Linux and MacOS look for cargo/registry and .cargo/registry
    if let Some(ix) = path
        .components()
        .position(|c| c.as_os_str() == "cargo" || c.as_os_str() == ".cargo")
        .and_then(|ix| path.components().nth(ix).zip(Some(ix)))
        .and_then(|(c, ix)| (c.as_os_str() == "registry").then_some(ix))
    {
        // It does what I want as far as *nix is concerned, might not work for Windows...
        #[allow(deprecated)]
        let mut source = std::env::home_dir().expect("No home dir?");

        source.push(".cargo");
        for part in path.components().skip(ix) {
            source.push(part);
        }
        if source.exists() {
            return Some(source);
        } else {
            panic!(
                "{path:?} looks like it can be a cargo registry reference but we failed to get it"
            );
        }
    }

    None
}

fn load_rust_sources<'a>(
    sysroot: &Path,
    statements: &'a [Statement],
    fmt: &Format,
    files: &mut BTreeMap<u64, (Cow<'a, Path>, Option<CachedLines>)>,
) {
    for line in statements {
        if let Statement::Directive(Directive::File(f)) = line {
            files.entry(f.index).or_insert_with(|| {
                let path = f.path.as_full_path();
                if fmt.verbosity > 1 {
                    safeprintln!("Reading file #{} {}", f.index, path.display());
                }

                if let Some(filepath) = locate_sources(sysroot, &path) {
                    let sources = std::fs::read_to_string(filepath).expect("Can't read a file");
                    let lines = CachedLines::without_ending(sources);
                    (path, Some(lines))
                } else {
                    if fmt.verbosity > 0 {
                        safeprintln!("File not found {}", path.display());
                    }
                    (path, None)
                }
            });
        }
    }
}

/// try to print `goal` from `path`, collect available items otherwise
pub fn dump_function(
    goal: ToDump,
    path: &Path,
    sysroot: &Path,
    fmt: &Format,
) -> anyhow::Result<()> {
    if fmt.verbosity > 2 {
        safeprintln!("goal: {goal:?}");
    }

    let contents = std::fs::read_to_string(path)?;
    let statements = parse_file(&contents)?;
    let functions = find_items(&statements);

    if fmt.verbosity > 2 {
        safeprintln!("{functions:?}");
    }

    let mut files = BTreeMap::new();
    if fmt.rust {
        load_rust_sources(sysroot, &statements, fmt, &mut files);
    }

    let dump_ctx = AsmDumpCtx {
        files: &files,
        fmt,
        stmts: &statements,
    };

    if let ToDump::Interactive = goal {
        interactive_mode(&functions, dump_ctx);
    } else {
        let range = get_dump_range(goal, fmt, functions);
        if fmt.verbosity > 0 && range.is_none() {
            safeprintln!("Going to print the whole file");
        }
        dump_ctx.dump_range(range)?;
    }
    Ok(())
}
