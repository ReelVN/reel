use super::*;
use crate::{position::{Encoding,LineIndex},workspace::{Workspace,file_uri}};
use std::{str::FromStr,time::Duration};
use lsp_server::{Connection,Message,Notification,Request,Response};
use lsp_types::*;
use serde_json::{Value,json};

fn position(source:&str,needle:&str,end:bool)->Position {
    let offset=source.rfind(needle).unwrap()+if end{needle.len()}else{0};
    LineIndex::new(source).position(source,offset,Encoding::Utf16).unwrap()
}
fn uri()->Uri {Uri::from_str("untitled:small.aside").unwrap()}
fn hover_text(value:Hover)->String {match value.contents {HoverContents::Markup(markup)=>markup.value,_=>panic!("expected Markdown hover")}}

#[test]
fn local_shadowing_docs_signature_and_recovery_use_original_scope() {
    let source="/// Adds a value.\nfn add(value:int, step:int=2)->int {value+step}\nfn main(){let value=1; {let value=\"inner\"; value;} add(value, step:3);}";
    let mut workspace=Workspace::new(ServerOptions::default());workspace.open(uri(),1,source.into()).unwrap();
    let local=workspace.complete(&uri(),position(source,"value;}",false),Encoding::Utf16).unwrap();
    let matches:Vec<_>=local.iter().filter(|item|item.label=="value").collect();
    assert_eq!(matches.len(),1);assert_eq!(matches[0].detail.as_deref(),Some("string"));
    let hover=hover_text(workspace.hover(&uri(),position(source,"add(value",false),Encoding::Utf16).unwrap().unwrap());
    assert!(hover.contains("Adds a value."));assert!(hover.contains("step: int = 2"));
    let signature=workspace.signature(&uri(),position(source,"step:3",true),Encoding::Utf16).unwrap().unwrap();
    assert_eq!(signature.active_parameter,Some(1));assert!(signature.signatures[0].label.contains("value: int"));
    let definition=workspace.definition(&uri(),position(source,"add(value",false),Encoding::Utf16).unwrap().unwrap();
    assert_eq!(definition.range.start,Position::new(1,3));
    assert_eq!(workspace.analyses,1,"unchanged queries share one project analysis");
    workspace.change(&uri(),2,vec![TextDocumentContentChangeEvent{range:None,range_length:None,text:"fn main(){let retained=1; retained. }".into()}],Encoding::Utf16).unwrap();
    assert!(workspace.diagnostics(&uri(),Encoding::Utf16).unwrap().iter().any(|params|!params.diagnostics.is_empty()));
    assert!(workspace.complete(&uri(),Position::new(0,33),Encoding::Utf16).unwrap().iter().any(|item|item.label=="retained"));
}

#[test]
fn unsaved_imports_missing_modules_and_close_invalidate_dependencies() {
    let root=tempfile::tempdir().unwrap();let main=root.path().join("main.aside");let helper=root.path().join("helpers.aside");
    let source="use game::helpers::add; fn main(){add(step:3);}";std::fs::write(&main,source).unwrap();
    let main_uri=file_uri(&main).unwrap();let helper_uri=file_uri(&helper).unwrap();
    let mut workspace=Workspace::new(ServerOptions{project_prefix:Some("game".into()),..Default::default()});
    workspace.open(main_uri.clone(),1,source.into()).unwrap();
    assert!(workspace.diagnostics(&main_uri,Encoding::Utf16).unwrap().iter().any(|params|!params.diagnostics.is_empty()));
    let helper_source="/// Actual unsaved helper.\npub fn add(step:int=7)->int {step}";
    workspace.open(helper_uri.clone(),1,helper_source.into()).unwrap();
    assert!(workspace.diagnostics(&main_uri,Encoding::Utf16).unwrap().iter().all(|params|params.diagnostics.is_empty()));
    let definition=workspace.definition(&main_uri,position(source,"add(step",false),Encoding::Utf16).unwrap().unwrap();assert_eq!(definition.uri,helper_uri);
    let hover=hover_text(workspace.hover(&main_uri,position(source,"add(step",false),Encoding::Utf16).unwrap().unwrap());
    assert!(hover.contains("Actual unsaved helper."));assert!(hover.contains("step: int = 7"));
    workspace.change(&helper_uri,2,vec![TextDocumentContentChangeEvent{range:None,range_length:None,text:"pub fn add(step:string)->string {step}".into()}],Encoding::Utf16).unwrap();
    assert!(workspace.diagnostics(&main_uri,Encoding::Utf16).unwrap().iter().any(|params|!params.diagnostics.is_empty()));
    workspace.close(&helper_uri);
    assert!(workspace.diagnostics(&main_uri,Encoding::Utf16).unwrap().iter().any(|params|params.diagnostics.iter().any(|diagnostic|diagnostic.code==Some(NumberOrString::String("ASS013".into())))));
}

#[test]
fn multiple_nearest_projects_and_public_reexports_keep_real_definition() {
    let root=tempfile::tempdir().unwrap();
    let mut workspace=Workspace::new(ServerOptions{project_prefix:Some("game".into()),module_roots:vec![ModuleRoot{prefix:String::new(),path:root.path().to_path_buf()}],..Default::default()});
    for (name,ty,value) in [("first","int","1"),("second","string","\"two\"")] {
        let project=root.path().join(name);std::fs::create_dir_all(project.join("ui")).unwrap();
        std::fs::write(project.join("main.aside"),"fn main(){}").unwrap();
        std::fs::write(project.join("helpers.aside"),format!("pub fn get()->{ty} {{{value}}}")).unwrap();
        std::fs::write(project.join("api.aside"),"pub use game::helpers::get;").unwrap();
        let uri=file_uri(&project.join("ui/scene.aside")).unwrap();let source="use game::api::get; fn scene(){let value=get(); value;}";
        workspace.open(uri.clone(),1,source.into()).unwrap();
        let hover=hover_text(workspace.hover(&uri,position(source,"value;",false),Encoding::Utf16).unwrap().unwrap());assert!(hover.contains(ty),"{hover}");
        assert_eq!(workspace.definition(&uri,position(source,"get();",false),Encoding::Utf16).unwrap().unwrap().uri,file_uri(&project.join("helpers.aside")).unwrap());
    }
}

#[test]
fn source_class_members_and_bound_method_parameters() {
    let source="class Counter {let value:int=1; fn add(self, amount:int=2)->int {self.value+amount}} fn main(){let counter=Counter(); counter.value; counter.add(amount:3);}";
    let mut workspace=Workspace::new(ServerOptions::default());workspace.open(uri(),1,source.into()).unwrap();
    let items=workspace.complete(&uri(),position(source,"counter.value",true),Encoding::Utf16).unwrap();
    assert!(items.iter().any(|item|item.label=="value"));assert!(items.iter().any(|item|item.label=="add"));
    let snapshot=workspace.snapshot(&uri()).unwrap();let module=snapshot.module().unwrap();
    for (i,node) in module.hir().nodes.iter().enumerate().filter(|(_,node)|matches!(node.kind,aside_compiler::hir::HirKind::Member|aside_compiler::hir::HirKind::Call)) {eprintln!("node {i} {:?} {:?} {:?}",module.hir().text(node.span),node.form,module.types()[module.facts()[i].ty.0 as usize]);}
    let signature=workspace.signature(&uri(),position(source,"amount:3",true),Encoding::Utf16).unwrap().unwrap();
    assert!(!signature.signatures[0].label.contains("self"));assert!(signature.signatures[0].label.contains("amount: int = 2"));
}

fn response(connection:&Connection,id:i32)->Response {
    loop {match connection.receiver.recv_timeout(Duration::from_secs(10)).unwrap() {Message::Response(response) if response.id==id.into()=>return response,_=>{}}}
}
fn diagnostics(connection:&Connection)->PublishDiagnosticsParams {
    loop {match connection.receiver.recv_timeout(Duration::from_secs(10)).unwrap() {Message::Notification(notification) if notification.method=="textDocument/publishDiagnostics"=>return serde_json::from_value(notification.params).unwrap(),_=>{}}}
}
#[test]
fn protocol_connection_initializes_edits_queries_closes_and_exits() {
    let (server,client)=Connection::memory();let thread=std::thread::spawn(||run_connection(server,ServerOptions::default()).unwrap());
    client.sender.send(Request::new(1.into(),"initialize".into(),json!({"capabilities":{"general":{"positionEncodings":["utf-16","utf-8"]}}})).into()).unwrap();
    let initialized=response(&client,1).result.unwrap();assert_eq!(initialized["capabilities"]["positionEncoding"],"utf-8");
    client.sender.send(Notification::new("initialized".into(),json!({})).into()).unwrap();
    client.sender.send(Notification::new("textDocument/didOpen".into(),json!({"textDocument":{"uri":uri(),"languageId":"aside","version":1,"text":"fn main(){中文;}"}})).into()).unwrap();
    let errors=diagnostics(&client);assert!(!errors.diagnostics.is_empty());
    client.sender.send(Notification::new("textDocument/didChange".into(),json!({"textDocument":{"uri":uri(),"version":2},"contentChanges":[{"text":"fn main(){let 中文=1; 中文;}"}]})).into()).unwrap();
    assert!(diagnostics(&client).diagnostics.is_empty());
    client.sender.send(Request::new(2.into(),"textDocument/completion".into(),json!({"textDocument":{"uri":uri()},"position":{"line":0,"character":27}})).into()).unwrap();
    let items=response(&client,2).result.unwrap();assert!(items.as_array().unwrap().iter().any(|item|item["label"]=="中文"));
    client.sender.send(Notification::new("textDocument/didClose".into(),json!({"textDocument":{"uri":uri()}})).into()).unwrap();assert!(diagnostics(&client).diagnostics.is_empty());
    client.sender.send(Request::new(3.into(),"shutdown".into(),Value::Null).into()).unwrap();assert!(response(&client,3).error.is_none());
    client.sender.send(Notification::new("exit".into(),Value::Null).into()).unwrap();thread.join().unwrap();
}
