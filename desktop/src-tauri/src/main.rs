#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    let root = daily_report::data_root();
    #[cfg(windows)]
    {
        use windows::{
            core::HSTRING,
            Win32::{
                Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS},
                System::Threading::CreateMutexW,
                UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK},
            },
        };
        let name = format!(
            "Local\\CodexDailyReport.{}.{}",
            daily_report::ROLE,
            daily_report::common::hash(&root.to_string_lossy())
        );
        let guard = unsafe { CreateMutexW(None, true, &HSTRING::from(name)) };
        let result = match guard {
            Ok(handle) => {
                if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                    unsafe {
                        let _ = CloseHandle(handle);
                        MessageBoxW(
                            None,
                            &HSTRING::from("程序已运行，请从托盘打开。"),
                            &HSTRING::from("Codex 工作日报"),
                            MB_OK,
                        );
                    }
                    return;
                }
                let result = daily_report::run();
                unsafe { let _ = CloseHandle(handle); }
                result
            }
            Err(error) => Err(error.into()),
        };
        if let Err(error) = result {
            unsafe {
                MessageBoxW(
                    None,
                    &HSTRING::from(format!("{error:#}")),
                    &HSTRING::from("Codex 工作日报"),
                    MB_OK | MB_ICONERROR,
                );
            }
        }
    }
    #[cfg(not(windows))]
    {
        // A second launch is handled by the single-instance plugin inside `run`, which
        // restores the hidden window instead of showing a modal message box.
        if let Err(error) = daily_report::run() {
            let _ = std::fs::create_dir_all(&root);
            let _ = std::fs::write(root.join("startup-error.txt"), format!("{error:#}"));
            eprintln!("启动失败：{error:#}");
            std::process::exit(1);
        }
    }
}
