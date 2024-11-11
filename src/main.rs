// #![allow(unused)]
#![allow(clippy::mutable_key_type)]

use lsp_types::{
    notification::{DidOpenTextDocument, Notification},
    request::{GotoDefinition, Request},
    DidOpenTextDocumentParams, GotoDefinitionParams, GotoDefinitionResponse, Location, OneOf,
    Position, Range, ServerCapabilities, Uri,
};
use smallvec::SmallVec;
use smol_str::SmolStr;
use std::{
    collections::{hash_map, HashMap},
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Component, Path, PathBuf},
};

use lsp_server::{Connection, ErrorCode, Message, RequestId, Response};

const HARE_PATH: &str = match option_env!("HARE_PATH") {
    Some(path) => path,
    None => "/usr/local/src/hare/stdlib/:/usr/local/src/hare/thirdparty/",
};

type Ident = SmallVec<[SmolStr; 4]>;
type DynError = Box<dyn core::error::Error + Sync + Send>;

fn main() -> Result<(), DynError> {
    env_logger::init();

    let (conn, io_threads) = Connection::stdio();

    let capabilities = ServerCapabilities {
        definition_provider: Some(OneOf::Left(true)),
        ..Default::default()
    };
    let server_capabilities = serde_json::to_value(capabilities).unwrap();
    let _initialization_params = conn.initialize(server_capabilities)?;
    let mut docs = HashMap::<Uri, Vec<String>>::new();

    let search_paths = HARE_PATH.split(':').collect::<Vec<_>>();

    loop {
        match conn.receiver.recv()? {
            Message::Request(request) => {
                if conn.handle_shutdown(&request)? {
                    eprintln!("Exiting...");
                    break;
                }
                match request.method.as_str() {
                    GotoDefinition::METHOD => {
                        let params = serde_json::from_value(request.params)?;
                        let resp = find_definition(params, &docs, request.id)?;
                        conn.sender.send(resp)?;
                    }
                    _ => {
                        eprintln!("INFO: ignoring request: {request:?}");
                    }
                };
            }
            Message::Response(response) => {
                eprintln!("{response:?}");
            }
            Message::Notification(notification) => match notification.method.as_str() {
                DidOpenTextDocument::METHOD => {
                    let params = serde_json::from_value(notification.params)?;
                    initialize_docs(params, &mut docs, &search_paths)?;
                }
                _ => {
                    eprintln!("INFO: ignoring notification: {notification:?}");
                }
            },
        }
    }

    io_threads.join()?;
    Ok(())
}

fn initialize_docs(
    params: DidOpenTextDocumentParams,
    docs: &mut HashMap<Uri, Vec<String>>,
    search_paths: &[&str],
) -> Result<(), DynError> {
    let lines = params
        .text_document
        .text
        .lines()
        .map(String::from)
        .collect::<Vec<_>>();
    let imports = get_imports(&lines);
    add_docs_from_imports(docs, &imports, search_paths)?;
    let doc_path = Path::new(params.text_document.uri.path().as_str());
    add_docs_from_directory(docs, doc_path.parent().unwrap())?;
    docs.insert(params.text_document.uri, lines);
    Ok(())
}

const PREFIXES: &[&str] = &[
    "export type",
    "export fn",
    "export def",
    "export let",
    "export const",
    "type",
    "fn",
    "def",
    "let",
    "const",
];

fn find_definition(
    params: GotoDefinitionParams,
    docs: &HashMap<Uri, Vec<String>>,
    id: RequestId,
) -> Result<Message, DynError> {
    let doc = params.text_document_position_params.text_document.uri;
    let loc = params.text_document_position_params.position;
    let imports = get_imports(&docs[&doc]);
    let doc_module = module_from_uri(&doc);
    if let Some(text) = docs.get(&doc) {
        if let Some(line) = text.get(loc.line as usize) {
            let ident = resolve_ident(
                doc_module.as_str(),
                &get_identifier(line, loc.character),
                &imports,
            );
            eprintln!("Resolved identifer under cursor: {ident:?}");
            let item_module = &ident[ident.len().saturating_sub(2)];
            for (uri, content) in module_files(docs, item_module.clone()) {
                eprintln!("searching in: {}", uri.path());
                if let Some(resp) = item_definition(uri, content, &ident)? {
                    return Ok(Response::new_ok(id.clone(), resp).into());
                }
            }
        }
    };
    Ok(Response::new_err(
        id.clone(),
        ErrorCode::RequestFailed as i32,
        "not found".to_string(),
    )
    .into())
}

fn get_imports(source: &[String]) -> Vec<Ident> {
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

fn item_definition(
    doc_uri: &Uri,
    doc_lines: &[String],
    ident: &Ident,
) -> Result<Option<GotoDefinitionResponse>, DynError> {
    for (ln, line) in doc_lines.iter().enumerate() {
        for p in PREFIXES {
            if let Some(s) = line.strip_prefix(p) {
                let actual = s
                    .trim()
                    .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .next()
                    .unwrap();
                let expected = ident.last().unwrap().clone();
                if actual == expected {
                    let col = line.find(actual).unwrap();

                    let resp = GotoDefinitionResponse::Scalar(Location {
                        uri: doc_uri.clone(),
                        range: Range::new(
                            Position::new(ln as _, col as _),
                            Position::new(ln as _, (col + expected.len()) as _),
                        ),
                    });

                    return Ok(Some(resp));
                }
            }
        }
    }
    // let mut module_path = PathBuf::from(path);
    // let ident = resolve_ident(ident, imports);
    // // eprintln!("resolvec ident: {ident:?}");
    // module_path.extend(&ident[..ident.len() - 1]);
    // let uri = path_to_uri(&module_path)?;
    // eprintln!("uri: {uri:?}");
    // // eprintln!("looking for: {}", module_path.display());
    // if module_path.is_dir() {
    //     // eprintln!("found the module at: {}", module_path.display());
    //     for file in fs::read_dir(module_path)?.flatten() {
    //         let filepath = file.path();
    //         if filepath.extension().is_some_and(|ext| ext == "ha") {
    //             eprintln!("checking {}", filepath.display());
    //             let content = fs::read_to_string(&filepath)?;
    //         }
    //     }
    // }
    Ok(None)
}

fn path_to_uri(filepath: &Path) -> Result<Uri, DynError> {
    let uri = format!("file://{}", filepath.display()).parse()?;
    Ok(uri)
}

fn get_identifier(line: &str, char_idx: u32) -> Ident {
    let i = char_idx as usize;
    let start = line[..i]
        .rfind(|c: char| !(c.is_alphanumeric() || c == ':' || c == '_'))
        .map(|j| j + 1)
        .unwrap_or_default();
    let end = line[i..]
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(|j| i + j)
        .unwrap_or(line.len());
    line[start..end].split("::").map(SmolStr::from).collect()
}

fn resolve_ident(current_module: &str, ident: &Ident, imports: &[Ident]) -> Ident {
    for import in imports.iter() {
        if import.last() == ident.first() {
            let mut ret = import.clone();
            ret.extend(ident[1..].iter().cloned());
            return ret;
        }
    }
    let mut ret = smallvec::smallvec![current_module.into()];
    ret.extend(ident.iter().cloned());
    ret
}

fn add_docs_from_directory(
    docs: &mut HashMap<Uri, Vec<String>>,
    dir_path: &Path,
) -> Result<(), DynError> {
    if dir_path.is_dir() {
        let Ok(dir) = fs::read_dir(dir_path) else {
            eprintln!("WARNING: could not open: {}", dir_path.display());
            return Ok(());
        };
        for entry in dir {
            let Ok(entry) = entry else { continue };
            let entry_path = entry.path();
            if entry_path.is_file() && entry_path.extension().is_some_and(|ext| ext == "ha") {
                let uri = path_to_uri(&entry_path)?;
                if let hash_map::Entry::Vacant(e) = docs.entry(uri) {
                    if let Ok(file) = File::open(&entry_path) {
                        // eprintln!("INFO: added doc: {}", entry_path.display());
                        e.insert(BufReader::new(file).lines().collect::<Result<_, _>>()?);
                    } else {
                        eprintln!("WARNING: could not open: {}", entry_path.display());
                    }
                }
            } else if entry_path.is_dir()
                && entry_path
                    .file_name()
                    .is_some_and(|name| name.as_encoded_bytes().starts_with(b"+"))
            {
                // eprintln!("INFO: indexing subdirectory: {}", entry_path.display());
                add_docs_from_directory(docs, &entry_path)?;
            }
        }
    }
    Ok(())
}

fn add_docs_from_imports(
    docs: &mut HashMap<Uri, Vec<String>>,
    imports: &[Ident],
    search_paths: &[&str],
) -> Result<(), DynError> {
    for path in search_paths.iter() {
        for import in imports.iter() {
            let mut module_path = PathBuf::from(path);
            module_path.extend(import);
            add_docs_from_directory(docs, &module_path)?;
        }
    }
    Ok(())
}

fn module_files(
    docs: &HashMap<Uri, Vec<String>>,
    current_module: SmolStr,
) -> impl Iterator<Item = (&Uri, &Vec<String>)> {
    docs.iter().filter_map(move |(k, v)| {
        let path = Path::new(k.path().as_str());
        if path.extension().is_none_or(|ext| ext != "ha") {
            return None;
        }
        let mut comps = path.components().rev();
        let _filename = comps.next()?;
        // current_module/foo.ha
        if let Component::Normal(parent) = comps.next()? {
            if parent == current_module.as_str() {
                return Some((k, v));
            }
            if parent.as_encoded_bytes().starts_with(b"+") {
                if let Component::Normal(parent) = comps.next()? {
                    if parent == current_module.as_str() {
                        return Some((k, v));
                    }
                }
            }
        }
        None
    })
}

fn module_from_uri(uri: &Uri) -> String {
    String::from_utf8_lossy(
        Path::new(uri.path().as_str())
            .parent()
            .expect("can't be root or empty")
            .file_name()
            .expect("uri does not terminate in `..`")
            .as_encoded_bytes(),
    )
    .into_owned()
}
