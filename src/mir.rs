use crate::{
    cached_lines::CachedLines,
    color, get_dump_range, interactive_mode,
    opts::{Format, ToDump},
    DumpRange, Item,
};
use owo_colors::OwoColorize;
use std::{collections::BTreeMap, io::Write, ops::Range, path::Path};

fn find_items(lines: &CachedLines) -> BTreeMap<Item, Range<usize>> {
    let mut res = BTreeMap::new();
    let mut current_item = None::<Item>;
    let mut block_start = None;

    for (ix, line) in lines.iter().enumerate() {
        if line.starts_with("//") {
            if block_start.is_none() {
                block_start = Some(ix);
            }
        } else if line == "}" {
            if let Some(mut cur) = current_item.take() {
                // go home clippy, you're drunk
                #[allow(clippy::range_plus_one)]
                let range = cur.len..ix + 1;
                cur.len = range.len();
                res.insert(cur, range);
            }
        } else if !(line.starts_with(' ') || line.is_empty()) && current_item.is_none() {
            let start = block_start.take().unwrap_or(ix);
            let mut name = line;
            'outer: loop {
                for suffix in [" {", " =", " -> ()"] {
                    if let Some(rest) = name.strip_suffix(suffix) {
                        name = rest;
                        continue 'outer;
                    }
                }
                break;
            }
            current_item = Some(Item {
                name: name.to_owned(),
                hashed: name.to_owned(),
                index: res.len(),
                len: start,
            });
        }
    }

    res
}

struct MirDumpCtx<'a> {
    #[allow(dead_code)]
    fmt: &'a Format,
    strings: &'a [&'a str],
}

impl DumpRange for MirDumpCtx<'_> {
    fn dump_range_into_writer(
        &self,
        range: Option<Range<usize>>,
        writer: &mut impl Write,
    ) -> anyhow::Result<()> {
        let strings = range.map_or(self.strings, |r| &self.strings[r]);

        for line in strings {
            if let Some(ix) = line.rfind("//") {
                writeln!(
                    writer,
                    "{}{}",
                    &line[..ix],
                    color!(&line[ix..], OwoColorize::cyan)
                )?;
            } else {
                writeln!(writer, "{line}")?;
            }
        }
        Ok(())
    }
}

/// dump mir code
///
/// # Errors
/// Reports file IO errors
pub fn dump_function(goal: ToDump, path: &Path, fmt: &Format) -> anyhow::Result<()> {
    let lines = CachedLines::without_ending(std::fs::read_to_string(path)?);
    let items = find_items(&lines);
    let strs = lines.iter().collect::<Vec<_>>();
    let dump_ctx = MirDumpCtx {
        fmt,
        strings: &strs,
    };
    if matches!(goal, ToDump::Interactive){
        interactive_mode(&items, dump_ctx);
    } else {
        dump_ctx.dump_range(get_dump_range(goal, fmt, items))?;
    }
    Ok(())
}
