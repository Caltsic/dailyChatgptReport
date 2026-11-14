pub mod common;
pub mod collector;
pub mod member;
#[cfg(feature="leader")]
pub mod leader;
#[cfg(feature="leader")]
pub mod documents;
#[cfg(feature="leader")]
mod documents_content;

use std::path::PathBuf;

pub const ROLE:&str=if cfg!(feature="leader"){"Leader"}else{"Member"};

/// Headless member core is usable without the desktop shell: the collector and the
/// upload client are plain Rust and are exercised by the Linux acceptance examples.
/// The desktop module below is the only place that depends on Tauri.
#[cfg(feature="desktop")]
mod desktop {
    use super::*;
    use crate::common::Result;
    use anyhow::{bail,Context};
    use serde_json::{json,Value};
    use std::sync::Arc;
    use tauri::{Manager,menu::{Menu,MenuItem},tray::TrayIconBuilder};

    pub struct AppState {
        pub root:PathBuf,
        #[cfg(not(feature="leader"))]
        pub member:Arc<crate::member::MemberClient>,
        #[cfg(feature="leader")]
        pub leader:Arc<crate::leader::Leader>,
        #[cfg(feature="leader")]
        pub documents:crate::documents::Documents,
    }
    impl AppState {
        pub fn new(root:PathBuf,resource:PathBuf)->Result<Self> {
            std::fs::create_dir_all(&root)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&root,std::fs::Permissions::from_mode(0o700))?;
            }
            Ok(Self{
                #[cfg(not(feature="leader"))]
                member:{let _=resource;crate::member::MemberClient::new(root.clone())?},
                #[cfg(feature="leader")]
                leader:crate::leader::Leader::new(root.clone(),resource)?,
                #[cfg(feature="leader")]
                documents:crate::documents::Documents::new(root.clone()),
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
                    Ok(json!({"role":ROLE,"version":env!("CARGO_PKG_VERSION"),"day":crate::common::today(zone)?,"root":self.root}))
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
                    // A source may be updated while the WebView prints. Do not deliver a stale summary as current.
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
        state.call(&app,&op,args.unwrap_or_else(||json!({}))).await.map_err(|e|crate::common::redact(&format!("{e:#}")))
    }
    fn open_path(path:&str)->Result<()> {
        let path=PathBuf::from(path).canonicalize()?;
        #[cfg(windows)]
        {
            use windows::{core::HSTRING,Win32::{UI::{Shell::ShellExecuteW,WindowsAndMessaging::SW_SHOWNORMAL}}};
            let result=unsafe{ShellExecuteW(None,&HSTRING::from("open"),&HSTRING::from(path.as_os_str()),None,None,SW_SHOWNORMAL)};
            if result.0 as isize<=32 {bail!("无法打开文件")};
        }
        #[cfg(not(windows))]
        {
            let status=std::process::Command::new("xdg-open").arg(&path).status().context("无法调用 xdg-open")?;
            if !status.success() {bail!("无法打开文件")};
        }
        Ok(())
    }
    pub fn run()->Result<()> {
        let root=crate::data_root();
        #[cfg(windows)]
        let builder=tauri::Builder::default();
        // Linux hides the window to the tray like Windows, but GNOME may not display tray
        // icons; letting a second launch focus the hidden window keeps it reachable.
        #[cfg(not(windows))]
        let builder=tauri::Builder::default().plugin(tauri_plugin_single_instance::init(|app,_argv,_cwd|{
            if let Some(window)=app.get_webview_window("main"){let _=window.show();let _=window.set_focus();}
        }));
        let builder=builder.plugin(tauri_plugin_dialog::init()).invoke_handler(tauri::generate_handler![dispatch]);
        builder.setup(move |app| {
            let resource=app.path().resource_dir()?;
            let state=Arc::new(AppState::new(root.clone(),resource)?);
            app.manage(state.clone());
            let open=MenuItem::with_id(app,"open","打开工作日报",true,None::<&str>)?;
            let exit=MenuItem::with_id(app,"exit","退出",true,None::<&str>)?;
            let menu=Menu::with_items(app,&[&open,&exit])?;
            let tray=TrayIconBuilder::new().icon(app.default_window_icon().context("缺少程序图标")?.clone()).tooltip(if ROLE=="Leader"{"Codex 工作日报 · 组长端"}else{"Codex 工作日报 · 成员端"}).menu(&menu).on_menu_event(|app,event|{
                match event.id.as_ref(){"exit"=>{
                    #[cfg(feature="leader")] {let app=app.clone();let state=app.state::<Arc<AppState>>().inner().clone();tauri::async_runtime::spawn(async move {state.leader.shutdown().await;app.exit(0);});}
                    #[cfg(not(feature="leader"))] app.exit(0);
                },"open"=>if let Some(w)=app.get_webview_window("main"){let _=w.show();let _=w.set_focus();},_=>{}}
            }).build(app);
            #[cfg(windows)]
            tray?;
            #[cfg(not(windows))]
            if let Err(error)=tray {
                // GNOME without the AppIndicator extension cannot show a tray icon. The member
                // app still runs; a second launch focuses the hidden window instead.
                let _=crate::common::atomic_text(&root.join("tray-warning.txt"),&crate::common::redact(&format!("托盘图标不可用：{error:#}")));
            }
            tauri::async_runtime::spawn(async move {if let Err(error)=state.start().await {let _=crate::common::atomic_text(&state.root.join("startup-error.txt"),&crate::common::redact(&error.to_string()));}});
            Ok(())
        }).on_window_event(|window,event|{
            if window.label()=="main" {if let tauri::WindowEvent::CloseRequested{api,..}=event {let _=window.hide();api.prevent_close();}}
        }).run(tauri::generate_context!()).context("启动桌面程序失败")
    }
}

#[cfg(feature="desktop")]
pub use desktop::run;

pub fn data_root()->PathBuf {
    let args:Vec<String>=std::env::args().collect();
    if let Some(i)=args.iter().position(|a|a=="--data-dir") { if let Some(path)=args.get(i+1){return PathBuf::from(path);} }
    #[cfg(windows)]
    {
        PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join("CodexDailyReport").join(ROLE)
    }
    #[cfg(not(windows))]
    {
        // Follow the XDG base directory spec so the member data stays in the usual place.
        let base=std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(||{
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
        });
        base.join("CodexDailyReport").join(ROLE)
    }
}
