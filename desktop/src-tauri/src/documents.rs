use crate::{common::{self,Result},documents_content};
use anyhow::{bail,Context};
use serde_json::{json,Value};
use std::{path::{Path,PathBuf},sync::{Arc,Mutex}};
use tauri::{AppHandle,WebviewWindowBuilder,WebviewUrl};
use tokio::sync::{Mutex as AsyncMutex,oneshot};

pub struct Documents { root:PathBuf,gate:AsyncMutex<()> }
pub struct Files { pub pdf:PathBuf,pub name:String }
impl Documents {
    pub fn new(root:PathBuf)->Self {Self{root,gate:AsyncMutex::new(())}}
    pub async fn export(&self,app:&AppHandle,report:&Value,department:bool)->Result<Files> {
        let _guard=self.gate.lock().await;
        let id=report["id"].as_str().context("报告缺少编号")?;
        uuid::Uuid::parse_str(id)?;
        let version=report["version"].as_u64().context("报告缺少版本")?;
        let base=self.root.join("reports").join(format!("{id}-V{version}"));
        std::fs::create_dir_all(base.parent().unwrap())?;
        let pdf=base.with_extension("pdf");
        let name=format!("{}-{}-V{}",report["day"].as_str().unwrap_or(""),if department{report["department"].as_str().unwrap_or("部门汇总")}else{report["memberName"].as_str().unwrap_or("日报")},version);
        let name=name.chars().map(|c|if "<>:\"/\\|?*".contains(c)||c.is_control(){'_'}else{c}).collect();
        // An archive is identified by its exact content, including original member feedback.
        let signature=common::hash(&serde_json::to_string(report)?);
        let marker=base.with_extension("content.sha256");
        if pdf.exists()&&std::fs::read_to_string(&marker).ok().as_deref()==Some(&signature) {return Ok(Files{pdf,name});}
        let html=documents_content::render(report,department)?;
        let temp=self.root.join("reports").join(format!(".export-{}",common::id()));
        let html_path=temp.with_extension("html");let pdf_temp=temp.with_extension("pdf");
        common::atomic_text(&html_path,&html)?;
        let result=async {
            print_pdf(app,&html_path,&pdf_temp).await?;
            std::fs::rename(&pdf_temp,&pdf)?;
            common::atomic_text(&marker,&signature)?;
            Ok::<_,anyhow::Error>(())
        }.await;
        for path in [&html_path,&pdf_temp]{if path.exists(){let _=std::fs::remove_file(path);}}
        result?;
        Ok(Files{pdf,name})
    }
    pub fn copy_to(&self,files:&Files,directory:Option<&str>)->Result<Value> {
        if let Some(directory)=directory.filter(|v|!v.is_empty()) {
            let folder=PathBuf::from(directory);std::fs::create_dir_all(&folder)?;
            let pdf=folder.join(format!("{}.pdf",files.name));
            if pdf!=files.pdf {std::fs::copy(&files.pdf,&pdf)?;}
            return Ok(json!({"pdf":pdf,"directory":folder}));
        }
        Ok(json!({"pdf":files.pdf,"directory":files.pdf.parent()}))
    }
}

type Completion=Arc<Mutex<Option<oneshot::Sender<std::result::Result<(),String>>>>>;
fn complete(sender:&Completion,result:std::result::Result<(),String>){if let Ok(mut value)=sender.lock(){if let Some(tx)=value.take(){let _=tx.send(result);}}}

pub async fn print_pdf(app:&AppHandle,html:&Path,output:&Path)->Result<()> {
    use webview2_com::{Microsoft::Web::WebView2::Win32::{ICoreWebView2_7,ICoreWebView2Environment6},NavigationCompletedEventHandler,PrintToPdfCompletedHandler};
    use windows::{core::{Interface,HSTRING,PWSTR},Win32::System::Com::CoTaskMemFree};
    let url=url::Url::from_file_path(html).map_err(|_|anyhow::anyhow!("文档路径无效"))?.to_string();
    let output=output.to_path_buf();
    let label=format!("print-{}",common::id());
    let window=WebviewWindowBuilder::new(app,&label,WebviewUrl::External("about:blank".parse()?)).title("正在导出 PDF").visible(false).build()?;
    let (tx,rx)=oneshot::channel();
    let sender:Completion=Arc::new(Mutex::new(Some(tx)));let sender_inner=sender.clone();
    let initialize=window.with_webview(move|webview|{
        let result=(||->windows::core::Result<()> {unsafe{
            let core=webview.controller().CoreWebView2()?;
            let settings=core.Settings()?;settings.SetIsScriptEnabled(false)?;
            let print=webview.environment().cast::<ICoreWebView2Environment6>()?.CreatePrintSettings()?;
            print.SetPageWidth(210.0/25.4)?;print.SetPageHeight(297.0/25.4)?;
            print.SetMarginTop(1.0)?;print.SetMarginBottom(1.0)?;print.SetMarginLeft(1.0)?;print.SetMarginRight(1.0)?;
            print.SetShouldPrintBackgrounds(true)?;print.SetShouldPrintHeaderAndFooter(false)?;
            print.SetHeaderTitle(&HSTRING::from(" "))?;print.SetFooterUri(&HSTRING::from(" "))?;
            let pdf_core=core.cast::<ICoreWebView2_7>()?;
            let source_core=core.clone();let target=url.clone();let done=sender_inner.clone();
            let handler=NavigationCompletedEventHandler::create(Box::new(move|_,args|{
                let result=(||->windows::core::Result<()> {
                    let mut source=PWSTR::null();source_core.Source(&mut source)?;
                    let loaded=source.to_string()?;CoTaskMemFree(Some(source.0 as *const _));
                    if loaded!=target {return Ok(());}
                    let mut successful=false.into();if let Some(args)=args{args.IsSuccess(&mut successful)?;}
                    if !successful.as_bool(){complete(&done,Err("PDF 模板加载失败".into()));return Ok(());}
                    let done_print=done.clone();
                    let callback=PrintToPdfCompletedHandler::create(Box::new(move|status,ok|{
                        complete(&done_print,status.map_err(|e|e.to_string()).and_then(|_|if ok{Ok(())}else{Err("WebView2 导出 PDF 失败".into())}));Ok(())
                    }));
                    pdf_core.PrintToPdf(&HSTRING::from(output.as_os_str()),&print,&callback)?;Ok(())
                })();
                if let Err(e)=result{complete(&done,Err(e.to_string()));}Ok(())
            }));
            let mut token=0i64;core.add_NavigationCompleted(&handler,&mut token)?;
            core.Navigate(&HSTRING::from(&url))?;Ok(())
        }})();
        if let Err(error)=result{complete(&sender_inner,Err(error.to_string()));}
    });
    if let Err(error)=initialize {let _=window.destroy();bail!("初始化 PDF 渲染失败：{error}");}
    let result=tokio::time::timeout(std::time::Duration::from_secs(120),rx).await;
    let _=window.destroy();
    result.context("PDF 导出超时")?.context("PDF 渲染窗口已关闭")?.map_err(anyhow::Error::msg)
}
