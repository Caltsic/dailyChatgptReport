use super::*;
use std::fs;
impl Leader {
    pub(super) fn backup(&self, directory: &Path) -> Result<PathBuf> {
        fs::create_dir_all(directory)?;
        let cfg = self.settings()?;
        let prefix = format!("department-{}-", &common::hash(s(&cfg, "id"))[..16]);
        let destination = directory.join(format!(
            "{}{}-{}.zip",
            prefix,
            chrono::Local::now().format("%Y%m%d-%H%M%S"),
            &common::id()[..6]
        ));
        let staging = directory.join(format!(".stage-{}", common::id()));
        fs::create_dir(&staging)?;
        let temp = destination.with_extension("zip.tmp");
        let result = (|| -> Result<()> {
            self.store.read(|source| {
                let mut target = Connection::open(staging.join("department.db"))?;
                {
                    let backup = rusqlite::backup::Backup::new(source, &mut target)?;
                    backup.run_to_completion(100, std::time::Duration::from_millis(10), None)?;
                }
                for folder in ["submissions", "reports"] {
                    fs::create_dir(staging.join(folder))?;
                    for entry in fs::read_dir(self.store.root.join(folder))? {
                        let entry = entry?;
                        if entry.file_type()?.is_file()
                            && !entry.file_name().to_string_lossy().ends_with(".tmp")
                        {
                            fs::copy(entry.path(), staging.join(folder).join(entry.file_name()))?;
                        }
                    }
                }
                sanitize(&target)?;
                fs::write(
                    staging.join("backup.json"),
                    serde_json::to_vec(
                        &json!({"format":1,"createdAt":common::now(),"department":cfg["name"]}),
                    )?,
                )?;
                Ok(())
            })?;
            let file = fs::File::create(&temp)?;
            let mut archive = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for file in walkdir::WalkDir::new(&staging) {
                let file = file?;
                if !file.file_type().is_file() {
                    continue;
                }
                let name = file
                    .path()
                    .strip_prefix(&staging)?
                    .to_string_lossy()
                    .replace('\\', "/");
                archive.start_file(name, options)?;
                let mut input = fs::File::open(file.path())?;
                std::io::copy(&mut input, &mut archive)?;
            }
            archive.finish()?.sync_all()?;
            fs::rename(&temp, &destination)?;
            let mut old = fs::read_dir(directory)?
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_type().map(|t| t.is_file()).unwrap_or(false)
                        && e.file_name().to_string_lossy().starts_with(&prefix)
                        && e.path().extension().is_some_and(|e| e == "zip")
                })
                .collect::<Vec<_>>();
            old.sort_by_key(|e| std::cmp::Reverse(e.metadata().and_then(|m| m.modified()).ok()));
            for entry in old.into_iter().skip(14) {
                fs::remove_file(entry.path())?;
            }
            Ok(())
        })();
        let _ = fs::remove_dir_all(&staging);
        let _ = fs::remove_file(&temp);
        result?;
        Ok(destination)
    }
    pub(super) fn restore(archive: &Path, destination: &Path) -> Result<PathBuf> {
        ensure!(!destination.as_os_str().is_empty(), "请选择恢复目标文件夹");
        let destination = if destination.is_absolute() {
            destination.to_path_buf()
        } else {
            std::env::current_dir()?.join(destination)
        };
        ensure!(
            !destination.exists()
                || (destination.is_dir() && fs::read_dir(&destination)?.next().is_none()),
            "为保留现有数据，恢复目标必须是空文件夹"
        );
        let parent = destination
            .parent()
            .ok_or_else(|| anyhow!("恢复目标无效"))?;
        fs::create_dir_all(parent)?;
        let staging = parent.join(format!(".restore-{}", common::id()));
        fs::create_dir(&staging)?;
        let result = (|| -> Result<()> {
            let mut zip = zip::ZipArchive::new(fs::File::open(archive)?)?;
            let mut expanded = 0u64;
            for i in 0..zip.len() {
                let mut entry = zip.by_index(i)?;
                if entry.is_dir() {
                    continue;
                }
                ensure!(!entry.is_symlink(), "备份路径无效");
                let name = entry.name().replace('\\', "/");
                let parts = name.split('/').collect::<Vec<_>>();
                ensure!(
                    parts
                        .iter()
                        .all(|p| !p.is_empty() && *p != "." && *p != ".." && !p.contains(':'))
                        && (name == "department.db"
                            || name == "backup.json"
                            || parts.len() == 2 && matches!(parts[0], "submissions" | "reports")),
                    "备份包含不支持的文件"
                );
                expanded = expanded
                    .checked_add(entry.size())
                    .ok_or_else(|| anyhow!("备份解压体积过大"))?;
                ensure!(expanded <= 20 * 1024 * 1024 * 1024, "备份解压体积过大");
                let path = staging.join(&name);
                fs::create_dir_all(path.parent().unwrap())?;
                let mut file = fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&path)?;
                std::io::copy(&mut entry, &mut file)?;
            }
            ensure!(
                staging.join("backup.json").is_file() && staging.join("department.db").is_file(),
                "不是有效的部门备份"
            );
            let metadata: Value = serde_json::from_slice(&fs::read(staging.join("backup.json"))?)?;
            ensure!(n(&metadata, "format") == 1, "备份格式不受支持");
            {
                let db = Connection::open(staging.join("department.db"))?;
                let integrity: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
                ensure!(integrity == "ok", "备份数据库完整性校验失败");
                ensure!(
                    get(&db, "DepartmentSettings", "settings")?.is_some(),
                    "备份缺少部门设置"
                );
                sanitize(&db)?;
                for snap in all(&db, "Snapshot", None)? {
                    load_capture_at(&staging, &snap)?;
                }
                db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")?;
            }
            fs::create_dir_all(staging.join("submissions"))?;
            fs::create_dir_all(staging.join("reports"))?;
            if destination.exists() {
                ensure!(
                    fs::read_dir(&destination)?.next().is_none(),
                    "恢复目标已出现其他文件，请选择空文件夹"
                );
                fs::remove_dir(&destination)?;
            }
            fs::rename(&staging, &destination)?;
            Ok(())
        })();
        if staging.exists() {
            let _ = fs::remove_dir_all(&staging);
        }
        result?;
        Ok(destination)
    }
}
fn sanitize(c: &Connection) -> Result<()> {
    c.execute(
        "DELETE FROM entities WHERE kind IN ('Device','InviteRecord')",
        [],
    )?;
    for mut j in all(c, "Job", None)?.into_iter().filter(pending) {
        j["state"] = json!("failed");
        j["error"] = json!("从备份恢复，请配置模型后重新生成");
        put(c, "Job", s(&j, "id"), &j, s(&j, "day"))?;
    }
    Ok(())
}
