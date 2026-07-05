use std::path::PathBuf;

const CLIENT_ICON_PNG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/nyx-client-icon.png"
));
const CLIENT_ICON_ICO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/nyx-client.ico"
));

pub fn cached_png_path() -> Option<PathBuf> {
    cache_icon_asset("nyx-client-icon.png", CLIENT_ICON_PNG)
}

pub fn cached_ico_path() -> Option<PathBuf> {
    cache_icon_asset("nyx-client.ico", CLIENT_ICON_ICO)
}

fn cache_icon_asset(file_name: &str, bytes: &[u8]) -> Option<PathBuf> {
    let mut path = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    path.push("NyxClient");
    std::fs::create_dir_all(&path).ok()?;
    path.push(file_name);

    let should_write = std::fs::read(&path)
        .map(|existing| existing != bytes)
        .unwrap_or(true);
    if should_write {
        std::fs::write(&path, bytes).ok()?;
    }

    Some(path)
}
