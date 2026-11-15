//! AI 咨询纯函数的测试：只测脱敏和地址校验，不联网、不读真实目录。

use std::path::Path;

use cclear_core::ai::{endpoint_url, redact};

#[test]
fn redact_hides_profile_and_username() {
    let profile = Path::new(r"C:\Users\Alice");

    // 反斜杠写法的用户目录整段被替换，大小写不敏感。
    assert_eq!(
        redact(r"c:\users\alice\AppData\Local\x", profile),
        r"%USERPROFILE%\AppData\Local\x"
    );

    // 单独出现的用户名（左右是分隔符）也要换掉。
    assert_eq!(
        redact(r"D:\Backup\ALICE\y", profile),
        r"D:\Backup\<用户名>\y"
    );

    // 只是同前缀的目录名，不是用户名本身，保持原样。
    assert_eq!(
        redact(r"D:\Backup\Alicex\y", profile),
        r"D:\Backup\Alicex\y"
    );

    // 和用户名无关的路径不动。
    assert_eq!(redact(r"G:\WSL\Ubuntu", profile), r"G:\WSL\Ubuntu");

    // 正斜杠写法的用户目录同样被替换。
    assert_eq!(
        redact("C:/Users/Alice/Downloads", profile),
        "%USERPROFILE%/Downloads"
    );

    // 句子里的路径被替换，其他文字原样保留。
    assert_eq!(
        redact(r"目录 C:\Users\Alice\Desktop 存在", profile),
        r"目录 %USERPROFILE%\Desktop 存在"
    );
}

#[test]
fn endpoint_url_rules() {
    // 没写到接口路径时补上。
    assert_eq!(
        endpoint_url("https://api.deepseek.com/v1").unwrap(),
        "https://api.deepseek.com/v1/chat/completions"
    );

    // 已经写了接口路径（末尾带斜杠）时去掉斜杠原样使用。
    assert_eq!(
        endpoint_url("https://x.com/v1/chat/completions/").unwrap(),
        "https://x.com/v1/chat/completions"
    );

    // 本机 http 允许，本地 Ollama 这类服务就是这种地址。
    assert_eq!(
        endpoint_url("http://localhost:11434/v1").unwrap(),
        "http://localhost:11434/v1/chat/completions"
    );

    // 方括号 IPv6 的本机地址同样允许。
    assert_eq!(
        endpoint_url("http://[::1]:8080/v1").unwrap(),
        "http://[::1]:8080/v1/chat/completions"
    );

    for rejected in [
        "http://192.168.1.2/v1",
        "http://api.x.com/v1",
        "ftp://x",
        "https://u:p@x.com/v1",
        "https://x.com/v1?k=1",
        // 没有主机名的地址不能放过。
        "https://",
        "https:///v1",
        // 方括号后面还接着别的字符，主机名就不再是本机了。
        "http://[::1]evil.com/v1",
    ] {
        assert!(
            endpoint_url(rejected).is_err(),
            "应当拒绝的地址被接受了：{rejected}"
        );
    }
}
