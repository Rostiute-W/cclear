fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "app_info",
            "candidate_locks",
            "open_candidate_in_explorer",
            "env_state",
            "plan_build",
            "plan_items",
            "plan_execute",
            "plan_cancel",
            "history_list",
            "history_detail",
            "rules_catalog",
            "rules_evaluate",
            "volume_info",
            "default_scan_roots",
            "scan_start",
            "scan_cancel",
            "scan_summary",
            "scan_children",
            "scan_top_files",
            "open_node_in_explorer",
            "open_top_file_in_explorer",
        ]),
    ))
    .expect("tauri-build 失败");
}
