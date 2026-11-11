// Native PDF export, destination copy and archive reuse for both report types.
use anyhow::ensure;
use daily_report::{common,documents::Documents};
use serde_json::{json,Value};
use std::path::PathBuf;
fn main(){
    let input=PathBuf::from(std::env::args().nth(1).expect("input directory"));
    let output=PathBuf::from(std::env::args().nth(2).expect("output directory"));
    std::fs::create_dir_all(&output).unwrap();
    let mut context=tauri::generate_context!();context.config_mut().app.windows.clear();
    tauri::Builder::default().setup(move|app|{
        let handle=app.handle().clone();
        tauri::async_runtime::spawn(async move{
            let result=async{
                let docs=Documents::new(output.clone());
                let mut results=Vec::new();
                for (file,department) in [("personal.json",false),("summary.json",true)]{
                    let mut report:Value=serde_json::from_slice(&std::fs::read(input.join(file))?)?;
                    if !department {report["finalizedAt"]=json!(common::now());}
                    let files=docs.export(&handle,&report,department).await?;
                    let original=std::fs::read(&files.pdf)?;
                    ensure!(original.starts_with(b"%PDF-"),"Missing PDF output");
                    let modified=std::fs::metadata(&files.pdf)?.modified()?;
                    let reused=docs.export(&handle,&report,department).await?;
                    ensure!(modified==std::fs::metadata(&reused.pdf)?.modified()?,"PDF-only archive was regenerated");
                    let destination=output.join("export");
                    let copied=docs.copy_to(&files,Some(destination.to_str().unwrap()))?;
                    ensure!(copied.get("word").is_none(),"Export response contains Word");
                    ensure!(std::fs::read(copied["pdf"].as_str().unwrap())?==original,"Export differs from archive");
                    results.push(copied);
                }
                for folder in [output.join("reports"),output.join("export")] {
                    for entry in std::fs::read_dir(folder)? {
                        ensure!(entry?.path().extension().and_then(|s|s.to_str())!=Some("docx"),"Unexpected Word file");
                    }
                }
                Ok::<_,anyhow::Error>(json!({"ok":true,"files":results,"verified":["personal and department PDF export","PDF-only archive reuse","copied PDF matches archive","no Word artifact or response field"]}))
            }.await;
            let (code,data)=match result{Ok(v)=>(0,v),Err(e)=>(1,json!({"ok":false,"error":format!("{e:#}")}))};
            let _=common::atomic_text(&output.join("result.json"),&serde_json::to_string_pretty(&data).unwrap());
            handle.exit(code);
        });Ok(())
    }).build(context).expect("native document check failed").run(|_,event|{
        if let tauri::RunEvent::ExitRequested{code:None,api,..}=event {api.prevent_exit();}
    });
}
