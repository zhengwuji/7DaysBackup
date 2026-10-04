use std::path::PathBuf;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        let icon = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("..")
            .join("resources")
            .join("icon.ico");
        if icon.exists() {
            res.set_icon(icon.to_str().unwrap());
        }
        // 图标嵌入失败不阻塞构建（exe 仍可用，只是无文件图标）
        let _ = res.compile();
    }
}
