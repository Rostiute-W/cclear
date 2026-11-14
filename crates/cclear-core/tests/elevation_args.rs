//! helper 命令行参数的校验：只接受内置操作和固定格式的路径。

use std::path::PathBuf;

use cclear_core::elevation::{SystemOp, command_line, parse_helper_args};
use uuid::Uuid;

/// 合法结果文件的样例。用户名和 UUID 都是假的，不碰真实路径。
const RESULT: &str =
    r"C:\Users\me\AppData\Local\CClear\elevated\00000000-0000-0000-0000-000000000000.json";
/// 合法进度文件的样例。
const PROGRESS: &str =
    r"C:\Users\me\AppData\Local\CClear\elevated\00000000-0000-0000-0000-000000000000.progress";
/// 合法隔离根目录的样例。
const QUARANTINE: &str = r"X:\CClearQuarantine";

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

#[test]
fn accepts_valid_arguments() {
    let uuid = Uuid::nil();
    let restore = format!("restore-batch:{uuid}");
    let purge = format!("purge-batch:{uuid}");

    let cases = vec![
        (
            args(&["--elevated", "--ops", "windows-temp", "--result", RESULT]),
            vec![SystemOp::WindowsTemp],
        ),
        (
            args(&[
                "--elevated",
                "--ops",
                "leappstore-cicada,nvidia-ota",
                "--result",
                RESULT,
                "--quarantine-root",
                QUARANTINE,
            ]),
            vec![SystemOp::LeAppStoreCicada, SystemOp::NvidiaOta],
        ),
        (
            args(&[
                "--elevated",
                "--ops",
                &restore,
                "--result",
                RESULT,
                "--progress",
                PROGRESS,
                "--quarantine-root",
                QUARANTINE,
            ]),
            vec![SystemOp::RestoreBatch(uuid)],
        ),
        (
            args(&[
                "--elevated",
                "--ops",
                &purge,
                "--result",
                RESULT,
                "--quarantine-root",
                QUARANTINE,
            ]),
            vec![SystemOp::PurgeBatch(uuid)],
        ),
    ];

    for (input, expected) in cases {
        let parsed = parse_helper_args(&input)
            .unwrap_or_else(|error| panic!("合法参数被拒绝：{input:?}，原因：{error}"));

        assert_eq!(parsed.ops, expected);
        assert_eq!(parsed.result, PathBuf::from(RESULT));
    }

    // 进度文件是可选的，给了合法值时要能保留下来。
    let parsed = parse_helper_args(&args(&[
        "--elevated",
        "--ops",
        "windows-temp",
        "--result",
        RESULT,
        "--progress",
        PROGRESS,
    ]))
    .expect("带进度文件的合法参数被拒绝");

    assert_eq!(parsed.progress, Some(PathBuf::from(PROGRESS)));
    assert_eq!(parsed.quarantine_root, None);
}

#[test]
fn accepts_username_with_space() {
    // Windows 用户名可以带空格，结果文件路径因此带空格；拼命令行时必须整体加引号。
    let spaced = format!(
        r"C:\Users\John Doe\AppData\Local\CClear\elevated\{}.json",
        Uuid::nil()
    );

    let parsed = parse_helper_args(&args(&[
        "--elevated",
        "--ops",
        "windows-temp",
        "--result",
        &spaced,
    ]))
    .expect("带空格用户名的合法参数被拒绝");

    assert_eq!(parsed.result, PathBuf::from(&spaced));

    let line = command_line(&[
        "--elevated".to_string(),
        "--ops".to_string(),
        "windows-temp".to_string(),
        "--result".to_string(),
        spaced.clone(),
    ]);

    assert_eq!(
        line,
        format!(r#"--elevated --ops windows-temp --result "{spaced}""#)
    );
}

#[test]
fn rejects_invalid_arguments() {
    let uuid = Uuid::nil();
    let restore = format!("restore-batch:{uuid}");
    let bad_batch = "restore-batch:not-a-uuid";
    let dotted = format!(
        r"C:\Users\..\AppData\Local\CClear\elevated\{}.json",
        Uuid::nil()
    );
    // Windows 也把 `/` 当路径分隔符，`a/..` 同样会穿越用户名段。
    let slashed = format!(
        r"C:\Users\a/..\AppData\Local\CClear\elevated\{}.json",
        Uuid::nil()
    );
    let outside = format!(r"C:\Users\me\AppData\Local\CClear\{}.json", Uuid::nil());
    let wrong_extension = format!(
        r"C:\Users\me\AppData\Local\CClear\elevated\{}.txt",
        Uuid::nil()
    );
    let short_uuid = r"C:\Users\me\AppData\Local\CClear\elevated\0000.json";
    let uppercase_uuid =
        r"C:\Users\me\AppData\Local\CClear\elevated\00000000-0000-0000-0000-00000000000A.json";

    let cases: Vec<(&str, Vec<String>)> = vec![
        (
            "未知 op",
            args(&["--elevated", "--ops", "unknown-op", "--result", RESULT]),
        ),
        (
            "重复 op",
            args(&[
                "--elevated",
                "--ops",
                "windows-temp,windows-temp",
                "--result",
                RESULT,
            ]),
        ),
        (
            "重复批次 op",
            args(&[
                "--elevated",
                "--ops",
                &format!("{restore},{restore}"),
                "--result",
                RESULT,
                "--quarantine-root",
                QUARANTINE,
            ]),
        ),
        (
            "批次 ID 不合法",
            args(&[
                "--elevated",
                "--ops",
                bad_batch,
                "--result",
                RESULT,
                "--quarantine-root",
                QUARANTINE,
            ]),
        ),
        (
            "result 扩展名不对",
            args(&[
                "--elevated",
                "--ops",
                "windows-temp",
                "--result",
                &wrong_extension,
            ]),
        ),
        (
            "result 带 ..",
            args(&["--elevated", "--ops", "windows-temp", "--result", &dotted]),
        ),
        (
            "result 的用户名段带 /",
            args(&["--elevated", "--ops", "windows-temp", "--result", &slashed]),
        ),
        (
            "result 不在 elevated 目录",
            args(&["--elevated", "--ops", "windows-temp", "--result", &outside]),
        ),
        (
            "result 的 UUID 太短",
            args(&[
                "--elevated",
                "--ops",
                "windows-temp",
                "--result",
                short_uuid,
            ]),
        ),
        (
            "result 的 UUID 里有大写",
            args(&[
                "--elevated",
                "--ops",
                "windows-temp",
                "--result",
                uppercase_uuid,
            ]),
        ),
        (
            "quarantine-root 带尾部分隔符",
            args(&[
                "--elevated",
                "--ops",
                "leappstore-cicada",
                "--result",
                RESULT,
                "--quarantine-root",
                r"X:\CClearQuarantine\",
            ]),
        ),
        (
            "quarantine-root 多一层",
            args(&[
                "--elevated",
                "--ops",
                "leappstore-cicada",
                "--result",
                RESULT,
                "--quarantine-root",
                r"X:\CClear\Quarantine",
            ]),
        ),
        (
            "quarantine-root 盘符小写",
            args(&[
                "--elevated",
                "--ops",
                "leappstore-cicada",
                "--result",
                RESULT,
                "--quarantine-root",
                r"x:\CClearQuarantine",
            ]),
        ),
        (
            "缺少 quarantine-root",
            args(&[
                "--elevated",
                "--ops",
                "leappstore-cicada",
                "--result",
                RESULT,
            ]),
        ),
        (
            "缺少 --elevated",
            args(&["--ops", "windows-temp", "--result", RESULT]),
        ),
        ("缺少 --ops", args(&["--elevated", "--result", RESULT])),
        (
            "缺少 --result",
            args(&["--elevated", "--ops", "windows-temp"]),
        ),
        (
            "ops 为空",
            args(&["--elevated", "--ops", "", "--result", RESULT]),
        ),
        (
            "不认识的参数",
            args(&[
                "--elevated",
                "--ops",
                "windows-temp",
                "--result",
                RESULT,
                "--force",
            ]),
        ),
        (
            "参数缺少取值",
            args(&["--elevated", "--ops", "windows-temp", "--result"]),
        ),
    ];

    for (label, input) in cases {
        assert!(
            parse_helper_args(&input).is_err(),
            "应当拒绝（{label}）：{input:?}"
        );
    }
}

#[test]
fn op_tokens_round_trip() {
    let uuid = Uuid::nil();
    let ops = vec![
        SystemOp::LeAppStoreCicada,
        SystemOp::NvidiaOta,
        SystemOp::WindowsTemp,
        SystemOp::WindowsLogs,
        SystemOp::SysReset,
        SystemOp::DismAnalyze,
        SystemOp::DismComponentCleanup,
        SystemOp::DeliveryOptimization,
        SystemOp::RestoreBatch(uuid),
        SystemOp::PurgeBatch(uuid),
    ];

    for op in ops {
        let token = op.token();
        let parsed = SystemOp::parse(&token)
            .unwrap_or_else(|error| panic!("操作 {token} 解析失败：{error}"));
        assert_eq!(parsed, op);
    }
}
