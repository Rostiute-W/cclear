//! `validate_external_path` 的表驱动测试。只校验字符串，不触碰磁盘。

use std::path::PathBuf;

use cclear_core::guard::validate_external_path;

#[test]
fn accepts_and_normalizes_absolute_paths() {
    let long_dir = format!(r"C:\{}-{}", "长目录".repeat(40), "x".repeat(200));
    let cases: Vec<(String, String)> = vec![
        // 盘符根目录保留末尾分隔符。
        (r"C:\".to_string(), r"C:\".to_string()),
        (r"D:\Windows".to_string(), r"D:\Windows".to_string()),
        (
            r"C:\Program Files\CClear".to_string(),
            r"C:\Program Files\CClear".to_string(),
        ),
        // 重复分隔符和小写盘符原样保留盘符，只压掉多余分隔符。
        (
            r"c:\\Windows\\System32".to_string(),
            r"c:\Windows\System32".to_string(),
        ),
        // 正斜杠统一成反斜杠。
        (
            r"C:/Users/中文/AppData".to_string(),
            r"C:\Users\中文\AppData".to_string(),
        ),
        // 末尾分隔符去掉。
        (r"C:\Users\张三\".to_string(), r"C:\Users\张三".to_string()),
        // 超过 MAX_PATH 的长路径同样接受。
        (long_dir.clone(), long_dir),
    ];

    for (input, expected) in cases {
        let actual = validate_external_path(&input).expect("合法路径应当通过校验");
        assert_eq!(actual, PathBuf::from(&expected), "输入：{input}");
    }
}

#[test]
fn rejects_paths_that_escape_the_drive() {
    let cases = [
        // UNC 与设备路径。
        r"\\server\share",
        r"\\?\C:\Windows",
        r"\\.\PhysicalDrive0",
        r"\\?\GLOBALROOT\Device\HarddiskVolume1",
        // 备用数据流。
        r"C:\Windows\notepad.exe:stream",
        // 相对路径与缺失分隔符。
        r"Windows\System32",
        r"..\Windows",
        r"C:Windows",
        r"C:",
        // 通配符。
        r"C:\Windows\*",
        r"C:\Win?ows",
        // 末尾点或末尾空格。
        r"C:\Windows.",
        r"C:\Windows ",
        // 点组件。
        r"C:\Windows\..\Users",
        r"C:\Windows\.\System32",
        // 控制字符。
        "C:\\Win\u{7}dows",
    ];

    for input in cases {
        assert!(
            validate_external_path(input).is_err(),
            "非法路径被放过了：{input}"
        );
    }
}
