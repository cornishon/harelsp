// #![allow(unused)]
#![allow(clippy::mutable_key_type)]

mod doc;
use crate::doc::Ident;
use crate::doc::{Document, HareItem};

use doc::get_identifier;
use lsp_types::{
    notification::{DidOpenTextDocument, Notification},
    request::{GotoDefinition, Request},
    CompletionOptions, DidOpenTextDocumentParams, GotoDefinitionParams, GotoDefinitionResponse,
    Location, OneOf, ServerCapabilities, Uri,
};
use smol_str::SmolStr;
use std::{
    collections::{hash_map, HashMap},
    path::{Component, Path, PathBuf},
};

use lsp_server::{Connection, ErrorCode, Message, RequestId, Response};

const HARE_PATH: &str = match option_env!("HARE_PATH") {
    Some(path) => path,
    None => "/usr/local/src/hare/stdlib/:/usr/local/src/hare/thirdparty/",
};

type DynError = Box<dyn core::error::Error + Sync + Send>;

fn main() -> Result<(), DynError> {
    env_logger::init();

    let (conn, io_threads) = Connection::stdio();

    let capabilities = ServerCapabilities {
        definition_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![":".into()]),
            ..Default::default()
        }),
        ..Default::default()
    };
    let server_capabilities = serde_json::to_value(capabilities).unwrap();
    let _initialization_params = conn.initialize(server_capabilities)?;
    let mut docs = HashMap::<Uri, Document>::new();

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
                        log::info!("ignoring request: {request:?}");
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

fn find_definition(
    params: GotoDefinitionParams,
    docs: &HashMap<Uri, Document>,
    id: RequestId,
) -> Result<Message, DynError> {
    let uri = params.text_document_position_params.text_document.uri;
    let loc = params.text_document_position_params.position;
    let doc_module = module_from_uri(&uri);
    if let Some(Document { lines, imports, .. }) = docs.get(&uri) {
        if let Some(line) = lines.get(loc.line as usize) {
            let ident = get_identifier(line, loc.character);
            let resolved_ident = resolve_ident(doc_module.as_str(), &ident, imports);
            let item_module = &resolved_ident[resolved_ident.len().saturating_sub(2)];
            for (uri, content) in module_files(docs, item_module.clone()) {
                if let Some(resp) = item_definition(uri, &content.items, &ident) {
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

fn item_definition(
    doc_uri: &Uri,
    items: &[HareItem],
    ident: &Ident,
) -> Option<GotoDefinitionResponse> {
    let expected = ident.last().unwrap().clone();
    let local = ident.len() == 1;
    for item in items {
        if item.name == expected && (local || item.exported) {
            return Some(GotoDefinitionResponse::Scalar(Location {
                uri: doc_uri.clone(),
                range: item.range,
            }));
        }
    }
    None
}

fn path_to_uri(filepath: &Path) -> Result<Uri, DynError> {
    let uri = format!("file://{}", filepath.display()).parse()?;
    Ok(uri)
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

pub fn initialize_docs(
    params: DidOpenTextDocumentParams,
    docs: &mut HashMap<Uri, Document>,
    search_paths: &[&str],
) -> Result<(), DynError> {
    let uri = params.text_document.uri;
    let root = Document::open(&uri)?;
    add_docs_from_imports(docs, &root.imports, search_paths)?;
    let doc_path = Path::new(uri.path().as_str());
    add_docs_from_directory(docs, doc_path.parent().unwrap())?;
    docs.insert(uri, root);
    Ok(())
}

fn add_docs_from_directory(
    docs: &mut HashMap<Uri, Document>,
    dir_path: &Path,
) -> Result<(), DynError> {
    if dir_path.is_dir() {
        let Ok(dir) = std::fs::read_dir(dir_path) else {
            eprintln!("WARNING: could not open: {}", dir_path.display());
            return Ok(());
        };
        for entry in dir {
            let Ok(entry) = entry else { continue };
            let entry_path = entry.path();
            if entry_path.is_file() && entry_path.extension().is_some_and(|ext| ext == "ha") {
                let uri = path_to_uri(&entry_path)?;
                if let hash_map::Entry::Vacant(e) = docs.entry(uri.clone()) {
                    if let Ok(doc) = Document::open(&uri) {
                        e.insert(doc);
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
    docs: &mut HashMap<Uri, Document>,
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
    docs: &HashMap<Uri, Document>,
    current_module: SmolStr,
) -> impl Iterator<Item = (&Uri, &Document)> {
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
