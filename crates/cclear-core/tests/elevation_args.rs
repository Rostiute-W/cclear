use cclear_core::elevation::{SystemOp, command_line, parse_helper_args};

const ID: &str = "9eecc303-1af5-4ecf-ac45-357253775275";

fn parse(
    parts: &[&str],
) -> Result<cclear_core::elevation::HelperArgs, cclear_core::elevation::ElevationError> {
    parse_helper_args(
        &parts
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>(),
    )
}

#[test]
fn accepts_fixed_batches_and_operation_parameters() {
    let parsed = parse(&[
        "--elevated",
        "--ops",
        "windows-temp,windows-logs,software-distribution",
        "--result",
        ID,
        "--temp-age",
        "72",
        "--reboot-delete",
    ])
    .unwrap();
    assert_eq!(
        parsed.ops,
        [
            SystemOp::WindowsTemp,
            SystemOp::WindowsLogs,
            SystemOp::SoftwareDistribution
        ]
    );
    assert_eq!(parsed.temp_age, 72);
    assert!(parsed.reboot_delete);
    let mft = parse(&[
        "--elevated",
        "--ops",
        "mft-scan",
        "--result",
        ID,
        "--mft-drive",
        "a",
    ])
    .unwrap();
    assert_eq!(mft.mft_drive, Some('A'));
}

#[test]
fn rejects_repeated_ops_and_untrusted_suffixes() {
    for ops in [
        "",
        "windows-temp,windows-temp",
        "windows-temp,other",
        "windows-temp,",
        "windows-temp:/path",
        "mft-scan",
        "reboot-delete",
    ] {
        assert!(
            parse(&["--elevated", "--ops", ops, "--result", ID]).is_err(),
            "{ops}"
        );
    }
    for result in [
        "",
        r"C:\Users\test\overwrite.json",
        r"..\overwrite",
        r"\\.\C:",
        "00000000-0000-0000-0000-000000000000",
    ] {
        assert!(
            parse(&["--elevated", "--ops", "windows-temp", "--result", result]).is_err(),
            "{result}"
        );
    }
    for extra in [
        ["--temp-age", "0"],
        ["--mft-drive", "C"],
        ["--result", ID],
        ["--output", "path"],
        ["--reboot-manifest", "9eecc303-1af5-4ecf-ac45-357253775276"],
    ] {
        let args = [
            "--elevated",
            "--ops",
            "windows-temp",
            "--result",
            ID,
            extra[0],
            extra[1],
        ];
        assert!(parse(&args).is_err(), "{extra:?}");
    }
    assert!(
        parse(&[
            "--elevated",
            "--ops",
            "mft-scan",
            "--result",
            ID,
            "--mft-drive",
            "C",
            "--reboot-delete",
        ])
        .is_err()
    );
}

#[test]
fn quotes_for_windows_argument_parsing() {
    assert_eq!(
        command_line(&["--result".into(), r"C:\A B\file.json".into(), String::new()]),
        r#"--result "C:\A B\file.json" """#
    );
    assert_eq!(command_line(&[r"C:\A B\".into()]), r#""C:\A B\\""#);
}
