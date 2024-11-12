use std::{
    fs::File,
    io::{self, BufRead, BufReader},
};

use lsp_types::{Position, Range, Uri};
use smallvec::SmallVec;
use smol_str::SmolStr;

pub type Ident = SmallVec<[SmolStr; 4]>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Document {
    pub lines: Vec<String>,
    pub imports: Vec<Ident>,
    pub items: Vec<HareItem>,
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

    pub fn get_documentation(&self, item: &HareItem) -> Option<String> {
        let item_line = item.range.start.line as usize;
        let start = self.lines[..item_line]
            .iter()
            .rposition(|line| !line.starts_with("//"))?;
        if start + 1 < item_line {
            Some(
                self.lines[start + 1..item_line]
                    .iter()
                    .map(|line| &line[2..])
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

pub fn parse_items(doc_lines: &[String]) -> Vec<HareItem> {
    let mut out = Vec::new();
    for (ln, line) in doc_lines.iter().enumerate() {
        for &(prefix, exported, kind) in PREFIXES {
            if let Some(s) = line.strip_prefix(prefix) {
                let name: SmolStr = s
                    .trim()
                    .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .next()
                    .unwrap()
                    .into();
                let start = line.find(name.as_str()).unwrap() as u32;
                let range = Range::new(
                    Position::new(ln as u32, start),
                    Position::new(ln as u32, start + name.len() as u32),
                );
                out.push(HareItem {
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

pub fn get_imports(source: &[String]) -> Vec<Ident> {
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
