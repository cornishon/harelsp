use std::{
    collections::HashSet,
    fs::File,
    io::{self, BufRead, BufReader},
};

use lsp_types::{Position, Range, Uri};
use smallvec::SmallVec;
use smol_str::SmolStr;

pub type Ident = SmallVec<[SmolStr; 4]>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub lines: Vec<String>,
    pub imports: HashSet<Ident>,
    pub items: HashSet<HareItem>,
}

impl Document {
    pub fn open(uri: &Uri) -> io::Result<Self> {
        let file = File::open(uri.path().as_str())?;
        // eprintln!("INFO: added doc: {}", entry_path.display());
        let lines = BufReader::new(file)
            .lines()
            .collect::<Result<Vec<String>, _>>()?;
        let items = parse_items(&lines);
        let imports = get_imports(&lines);
        Ok(Document {
            lines,
            items,
            imports,
        })
    }

    pub fn new(lines: Vec<String>) -> Self {
        let items = parse_items(&lines);
        let imports = get_imports(&lines);
        Document {
            lines,
            items,
            imports,
        }
    }

    pub fn get_documentation(&self, item: &HareItem) -> Option<String> {
        let item_line = item.range.start.line as usize;
        let start = self.lines[..item_line]
            .iter()
            .rposition(|line| !line.starts_with("//"))?;
        if start + 1 < item_line {
            Some(
                self.lines[start + 1..item_line]
                    .iter()
                    .flat_map(|line| [&line[2..], "\n"])
                    .collect(),
            )
        } else {
            None
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum HareKind {
    Type,
    Fn,
    Def,
    Var,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HareItem {
    pub kind: HareKind,
    pub name: SmolStr,
    pub range: Range,
    pub exported: bool,
}

pub const PREFIXES: &[(&str, bool, HareKind)] = &[
    ("export type", true, HareKind::Type),
    ("export fn", true, HareKind::Fn),
    ("export def", true, HareKind::Def),
    ("export let", true, HareKind::Var),
    ("export const", true, HareKind::Var),
    ("type", false, HareKind::Type),
    ("fn", false, HareKind::Fn),
    ("def", false, HareKind::Def),
    ("let", false, HareKind::Var),
    ("const", false, HareKind::Var),
];

pub fn parse_items(doc_lines: &[String]) -> HashSet<HareItem> {
    let mut out = HashSet::new();
    for (ln, line) in doc_lines.iter().enumerate() {
        for &(mut prefix, exported, kind) in PREFIXES {
            if let Some(at_idx) = line.find("@symbol") {
                let symbol_end = at_idx + line[at_idx..].find(')').unwrap();
                prefix = &line[..prefix.len() + symbol_end - at_idx];
                // log::info!("{prefix}");
            };
            if let Some(s) = line.strip_prefix(prefix) {
                let name: SmolStr = s
                    .trim()
                    .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .next()
                    .unwrap()
                    .into();
                let start = line[prefix.len()..].find(name.as_str()).unwrap() + prefix.len();
                let range = Range::new(
                    Position::new(ln as u32, start as u32),
                    Position::new(ln as u32, start as u32 + name.len() as u32),
                );
                out.insert(HareItem {
                    kind,
                    name,
                    range,
                    exported,
                });
            }
        }
    }
    out
}

pub fn get_imports(source: &[String]) -> HashSet<Ident> {
    source
        .iter()
        .filter_map(|l| {
            if let Some(s) = l.strip_prefix("use") {
                let end = s.find(';')? as u32;
                Some(get_identifier(s, end - 1))
            } else {
                None
            }
        })
        .collect()
}

pub fn get_identifier(line: &str, char_idx: u32) -> Ident {
    let i = char_idx as usize;
    let start = line[..i]
        .rfind(|c: char| !(c.is_alphanumeric() || c == ':' || c == '_'))
        .map(|j| j + 1)
        .unwrap_or_default();
    let end = line[i..]
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(|j| i + j)
        .unwrap_or(line.len());
    line[start..end]
        .trim_end_matches(':')
        .split("::")
        .map(SmolStr::from)
        .collect()
}
