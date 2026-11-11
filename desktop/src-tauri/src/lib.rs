pub mod common;
pub mod collector;
pub mod member;
#[cfg(feature="leader")]
pub mod leader;
#[cfg(feature="leader")]
pub mod documents;
#[cfg(feature="leader")]
mod documents_content;

use std::{path::PathBuf,sync::Arc};
use anyhow::{bail,Context};
use serde_json::{json,Value};
use tauri::{Manager,menu::{Menu,MenuItem},tray::TrayIconBuilder};
use common::Result;

pub const ROLE:&str=if cfg!(feature="leader"){"Leader"}else{"Member"};
pub struct AppState {
    pub root:PathBuf,
    #[cfg(not(feature="leader"))]
    pub member:Arc<member::MemberClient>,
    #[cfg(feature="leader")]
    pub leader:Arc<leader::Leader>,
    #[cfg(feature="leader")]
    pub documents:documents::Documents,
}
impl AppState {
    pub fn new(root:PathBuf,resource:PathBuf)->Result<Self> {
        std::fs::create_dir_all(&root)?;
        Ok(Self{
            #[cfg(not(feature="leader"))]
            member:{let _=resource;member::MemberClient::new(root.clone())?},
            #[cfg(feature="leader")]
            leader:leader::Leader::new(root.clone(),resource)?,
            #[cfg(feature="leader")]
            documents:documents::Documents::new(root.clone()),
            root,
        })
    }
    pub async fn start(&self)->Result<()> {
        #[cfg(not(feature="leader"))] self.member.clone().start();
        #[cfg(feature="leader")] self.leader.clone().start().await?;
        Ok(())
    }
    pub async fn call(&self,app:&tauri::AppHandle,op:&str,args:Value)->Result<Value> {
        match op {
            "app.info" => {
                #[cfg(feature="leader")]
                let info=self.leader.dispatch("leader.init",json!({})).await?;
                #[cfg(not(feature="leader"))]
                let info=self.member.dispatch("member.init",json!({})).await?;
                let zone=info.pointer("/settings/timeZone").or_else(||info.pointer("/pairing/identity/timeZone")).and_then(Value::as_str).unwrap_or("Asia/Shanghai");
                Ok(json!({"role":ROLE,"version":env!("CARGO_PKG_VERSION"),"day":common::today(zone)?,"root":self.root}))
            },
            "app.openPath" => { open_path(args["path"].as_str().context("请选择文件")?)?;Ok(Value::Null) },
            #[cfg(feature="leader")]
            "app.exportPersonal" => {
                let report=self.leader.dispatch("leader.prepareExport",args.clone()).await?;
                let files=self.documents.export(app,&report,false).await?;
                self.leader.dispatch("leader.commitExport",json!({"report":report})).await?;
                self.documents.copy_to(&files,args["directory"].as_str())
            },
            #[cfg(feature="leader")]
            "app.exportSummary" => {
                let report=self.leader.dispatch("leader.getSummary",args.clone()).await?;
                let files=self.documents.export(app,&report,true).await?;
                // A source may be updated while WebView2 prints. Do not deliver a stale summary as current.
                self.leader.dispatch("leader.getSummary",args.clone()).await?;
                self.documents.copy_to(&files,args["directory"].as_str())
            },
            #[cfg(not(feature="leader"))]
            _ if op.starts_with("member.") => {let _=app;self.member.dispatch(op,args).await},
            #[cfg(feature="leader")]
            _ if op.starts_with("leader.") => self.leader.dispatch(op,args).await,
            _ => bail!("当前端不支持此操作：{op}"),
        }
    }
}
#[tauri::command]
async fn dispatch(app:tauri::AppHandle,state:tauri::State<'_,Arc<AppState>>,op:String,args:Option<Value>)->std::result::Result<Value,String>{
    state.call(&app,&op,args.unwrap_or_else(||json!({}))).await.map_err(|e|common::redact(&format!("{e:#}")))
}
fn open_path(path:&str)->Result<()> {
    use windows::{core::HSTRING,Win32::{UI::{Shell::ShellExecuteW,WindowsAndMessaging::SW_SHOWNORMAL}}};
    let path=PathBuf::from(path).canonicalize()?;
    let result=unsafe{ShellExecuteW(None,&HSTRING::from("open"),&HSTRING::from(path.as_os_str()),None,None,SW_SHOWNORMAL)};
    if result.0 as isize<=32 {bail!("无法打开文件")};Ok(())
}
pub fn data_root()->PathBuf {
    let args:Vec<String>=std::env::args().collect();
    if let Some(i)=args.iter().position(|a|a=="--data-dir") { if let Some(path)=args.get(i+1){return PathBuf::from(path);} }
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join("CodexDailyReport").join(ROLE)
}
pub fn run()->Result<()> {
    let root=data_root();
    let builder=tauri::Builder::default().plugin(tauri_plugin_dialog::init()).invoke_handler(tauri::generate_handler![dispatch]);
    builder.setup(move |app| {
        let resource=app.path().resource_dir()?;
        let state=Arc::new(AppState::new(root.clone(),resource)?);
        app.manage(state.clone());
        let open=MenuItem::with_id(app,"open","打开工作日报",true,None::<&str>)?;
        let exit=MenuItem::with_id(app,"exit","退出",true,None::<&str>)?;
        let menu=Menu::with_items(app,&[&open,&exit])?;
        TrayIconBuilder::new().icon(app.default_window_icon().context("缺少程序图标")?.clone()).tooltip(if ROLE=="Leader"{"Codex 工作日报 · 组长端"}else{"Codex 工作日报 · 成员端"}).menu(&menu).on_menu_event(|app,event|{
            match event.id.as_ref(){"exit"=>{
                #[cfg(feature="leader")] {let app=app.clone();let state=app.state::<Arc<AppState>>().inner().clone();tauri::async_runtime::spawn(async move {state.leader.shutdown().await;app.exit(0);});}
                #[cfg(not(feature="leader"))] app.exit(0);
            },"open"=>if let Some(w)=app.get_webview_window("main"){let _=w.show();let _=w.set_focus();},_=>{}}
        }).build(app)?;
        tauri::async_runtime::spawn(async move {if let Err(error)=state.start().await {let _=common::atomic_text(&state.root.join("startup-error.txt"),&common::redact(&error.to_string()));}});
        Ok(())
    }).on_window_event(|window,event|{
        if window.label()=="main" {if let tauri::WindowEvent::CloseRequested{api,..}=event {let _=window.hide();api.prevent_close();}}
    }).run(tauri::generate_context!()).context("启动桌面程序失败")
}
