// #![allow(unused)]
#![allow(clippy::mutable_key_type)]

mod doc;
use crate::doc::{get_identifier, Document, HareItem, Ident};

use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
};

use lsp_server::{Connection, Message, Response};
use lsp_types::{
    notification::{DidChangeTextDocument, DidOpenTextDocument, Notification},
    request::{Completion, GotoDefinition, HoverRequest, Request},
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, Documentation, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverContents, HoverParams, Location, MarkedString, OneOf,
    ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
};
use smol_str::SmolStr;

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
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(true.into()),
        ..Default::default()
    };
    let server_capabilities = serde_json::to_value(capabilities)?;
    let _initialization_params = conn.initialize(server_capabilities)?;
    let mut docs = HashMap::<Uri, Document>::new();

    let harepath: String = std::env::var("HAREPATH")
        .unwrap_or("/usr/local/src/hare/stdlib/:/usr/local/src/hare/third-party/".to_owned());

    let search_paths = harepath.split(':').collect::<Vec<_>>();

    loop {
        match conn.receiver.recv()? {
            Message::Request(request) => {
                if conn.handle_shutdown(&request)? {
                    log::info!("Exiting...");
                    break;
                }
                match request.method.as_str() {
                    GotoDefinition::METHOD => {
                        let params = serde_json::from_value(request.params)?;
                        let defs = find_definition(params, &docs);
                        conn.sender
                            .send(Response::new_ok(request.id, defs).into())?;
                    }
                    Completion::METHOD => {
                        let params = serde_json::from_value(request.params)?;
                        let completions = generate_completions(params, &docs);
                        conn.sender
                            .send(Response::new_ok(request.id, completions).into())?;
                    }
                    HoverRequest::METHOD => {
                        let params = serde_json::from_value(request.params)?;
                        let hover = generate_hover(params, &docs);
                        conn.sender
                            .send(Response::new_ok(request.id, hover).into())?;
                    }
                    _ => {
                        log::info!("ignoring request: {request:?}");
                    }
                };
            }
            Message::Response(response) => {
                log::info!("{response:?}");
            }
            Message::Notification(notification) => match notification.method.as_str() {
                DidOpenTextDocument::METHOD => {
                    let params = serde_json::from_value(notification.params)?;
                    initialize_docs(params, &mut docs, &search_paths)?;
                }
                DidChangeTextDocument::METHOD => {
                    let params = serde_json::from_value(notification.params)?;
                    update_docs(params, &mut docs, &search_paths)?;
                }
                _ => {
                    log::info!("ignoring notification: {notification:?}");
                }
            },
        }
    }

    io_threads.join()?;
    Ok(())
}

fn generate_hover(params: HoverParams, docs: &HashMap<Uri, Document>) -> Option<Hover> {
    let uri = params.text_document_position_params.text_document.uri;
    let loc = params.text_document_position_params.position;
    let doc_module = module_from_uri(&uri);
    if let Some(Document { lines, imports, .. }) = docs.get(&uri) {
        let ident = get_identifier(&lines[loc.line as usize], loc.character);
        let item_module = module_of_ident(&ident, &doc_module, imports);
        for (_uri, module) in module_files(docs, item_module) {
            if let Some(item) = find_item(&module.items, &ident) {
                return module.get_documentation(item).map(|d| Hover {
                    contents: HoverContents::Scalar(MarkedString::String(d)),
                    range: Some(item.range),
                });
            }
        }
    }
    None
}

fn generate_completions(
    params: CompletionParams,
    docs: &HashMap<Uri, Document>,
) -> CompletionResponse {
    let uri = params.text_document_position.text_document.uri;
    let loc = params.text_document_position.position;
    let doc_module = module_from_uri(&uri);
    let mut completions = Vec::new();
    if let Some(Document { lines, imports, .. }) = docs.get(&uri) {
        let ident = get_identifier(&lines[loc.line as usize], loc.character);
        let item_module = module_of_ident(&ident, &doc_module, imports);
        for (_uri, module) in module_files(docs, item_module) {
            completions.extend(module.items.iter().map(|item| CompletionItem {
                label: item.name.to_string(),
                kind: match item.kind {
                    doc::HareKind::Type => Some(CompletionItemKind::STRUCT),
                    doc::HareKind::Fn => Some(CompletionItemKind::FUNCTION),
                    doc::HareKind::Def => Some(CompletionItemKind::CONSTANT),
                    doc::HareKind::Var => Some(CompletionItemKind::VARIABLE),
                },
                documentation: module.get_documentation(&item).map(Documentation::String),
                ..Default::default()
            }));
        }
    }
    CompletionResponse::Array(completions)
}

fn module_of_ident(ident: &Ident, current_module: &str, imports: &HashSet<Ident>) -> SmolStr {
    let resolved_ident = resolve_ident(current_module, &ident, imports);
    let item_module = &resolved_ident[resolved_ident.len().saturating_sub(2)];
    item_module.clone()
}

fn find_definition(
    params: GotoDefinitionParams,
    docs: &HashMap<Uri, Document>,
) -> GotoDefinitionResponse {
    let uri = params.text_document_position_params.text_document.uri;
    let loc = params.text_document_position_params.position;
    let doc_module = module_from_uri(&uri);
    let mut locations = Vec::with_capacity(4);
    if let Some(Document { lines, imports, .. }) = docs.get(&uri) {
        if let Some(line) = lines.get(loc.line as usize) {
            let ident = get_identifier(line, loc.character);
            let resolved_ident = resolve_ident(doc_module.as_str(), &ident, imports);
            let item_module = &resolved_ident[resolved_ident.len().saturating_sub(2)];
            for (uri, content) in module_files(docs, item_module.clone()) {
                if let Some(item) = find_item(&content.items, &ident) {
                    locations.push(Location {
                        uri: uri.clone(),
                        range: item.range,
                    });
                }
            }
        }
    };
    if locations.len() == 1 {
        GotoDefinitionResponse::Scalar(locations.pop().unwrap())
    } else {
        GotoDefinitionResponse::Array(locations)
    }
}

fn find_item<'i>(items: &'i HashSet<HareItem>, ident: &Ident) -> Option<&'i HareItem> {
    let expected = ident.last().unwrap().clone();
    let local = ident.len() == 1;
    for item in items {
        if item.name == expected && (local || item.exported) {
            return Some(item);
        }
    }
    None
}

fn path_to_uri(filepath: &Path) -> Result<Uri, DynError> {
    let uri = format!("file://{}", filepath.display()).parse()?;
    Ok(uri)
}

fn resolve_ident(current_module: &str, ident: &Ident, imports: &HashSet<Ident>) -> Ident {
    for import in imports.iter() {
        if import.last() == ident.first() {
            return import
                .clone()
                .into_iter()
                .chain(ident[1..].iter().cloned())
                .collect();
        }
    }
    [SmolStr::from(current_module)]
        .into_iter()
        .chain(ident.iter().cloned())
        .collect()
}

pub fn initialize_docs(
    params: DidOpenTextDocumentParams,
    docs: &mut HashMap<Uri, Document>,
    search_paths: &[&str],
) -> Result<(), DynError> {
    let uri = params.text_document.uri;
    let root = Document::open(&uri)?;
    add_docs_from_imports(docs, root.imports.iter(), search_paths)?;
    let doc_path = Path::new(uri.path().as_str());
    add_docs_from_directory(docs, doc_path.parent().unwrap())?;
    docs.insert(uri, root);
    Ok(())
}

pub fn update_docs(
    params: DidChangeTextDocumentParams,
    docs: &mut HashMap<Uri, Document>,
    search_paths: &[&str],
) -> Result<(), DynError> {
    let uri = params.text_document.uri;
    log::info!("{:?}", params.content_changes);
    if let Some(doc) = docs.remove(&uri) {
        // let mut lines = doc.lines;
        // for change in params.content_changes.iter() {
        //     let range = change.range.unwrap_or_default();
        //     let start = range.start.line as usize;
        //     let end = range.end.line as usize;
        //     assert_eq!(range.start.character, 0);
        //     assert_eq!(range.end.character, 0);
        //     let changed_lines = change.text.lines().map(String::from);
        //     lines.splice(start..end, changed_lines);
        // }
        assert!(params.content_changes.len() == 1);
        let updated_doc = Document::new(
            params.content_changes[0]
                .text
                .lines()
                .map(String::from)
                .collect(),
        );
        add_docs_from_imports(
            docs,
            updated_doc.imports.difference(&doc.imports),
            search_paths,
        )?;
        docs.insert(uri, updated_doc);
    };
    Ok(())
}

fn add_docs_from_directory(
    docs: &mut HashMap<Uri, Document>,
    dir_path: &Path,
) -> Result<(), DynError> {
    if dir_path.is_dir() {
        let Ok(dir) = std::fs::read_dir(dir_path) else {
            log::warn!("could not open: {}", dir_path.display());
            return Ok(());
        };
        for entry in dir {
            let Ok(entry) = entry else { continue };
            let entry_path = entry.path();
            if entry_path.is_file() && entry_path.extension().is_some_and(|ext| ext == "ha") {
                let uri = path_to_uri(&entry_path)?;
                if let Ok(doc) = Document::open(&uri) {
                    docs.insert(uri, doc);
                } else {
                    eprintln!("WARNING: could not open: {}", entry_path.display());
                }
            } else if entry_path.is_dir()
                && entry_path
                    .file_name()
                    .is_some_and(|name| name.as_encoded_bytes().starts_with(b"+"))
            {
                // log::info!("indexing subdirectory: {}", entry_path.display());
                add_docs_from_directory(docs, &entry_path)?;
            }
        }
    }
    Ok(())
}

fn add_docs_from_imports<'i, I: Iterator<Item = &'i Ident>>(
    docs: &mut HashMap<Uri, Document>,
    imports: I,
    search_paths: &[&str],
) -> Result<(), DynError> {
    for import in imports {
        for path in search_paths.iter() {
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
