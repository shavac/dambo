fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "get_plugins",
            "open_plugin_desktop",
            "close_plugin_desktop",
            "open_plugins_dir",
            "open_folder",
            "open_in_browser",
            "set_always_on_top",
            "get_i18n",
            "refresh_qr_code",
        ]),
    ))
    .expect("failed to run tauri-build");
}
