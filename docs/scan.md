# C 盘只读扫描结果（非管理员权限，部分系统目录不可读）

- C: 已用 165.6 GB，剩余 34.4 GB；当前进程非管理员
- 根目录：swapfile.sys 0.25 GB；pagefile/hiberfil 未读到（可能需要管理员权限）

## 顶层
| 大小 | 路径 |
|---|---|
| 72.4 GB | C:\Users（几乎全是 ZhuanZ：71.9 GB） |
| 50.7 GB | C:\Windows（WinSxS 20.5 GB 含硬链接，大小被高估；DriverStore 6.6 GB；Installer 8.5 GB） |
| 21.4 GB | C:\Program Files |
| 10.0 GB | C:\Program Files (x86) |
| 7.8 GB | C:\ProgramData（Package Cache 1.3 GB） |

## 用户目录（C:\Users\ZhuanZ）
- AppData\Local 24.8 GB：Programs 4.1、Microsoft 4.0（Edge User Data 1.6、Explorer 缩略图缓存 0.23）、MathWorks 2.4、uv\cache 1.7、wsl 1.4、OpenAI 1.1、PowerToys 0.95、Temp 0.76、qianfan-desktop-app-updater 0.73、balena_etcher 0.68、NGR 0.63、Packages 0.58、Google(Chrome) 0.49、Package Cache 0.37、Quark 0.30、npm-cache 0.28
- AppData\Roaming 19.1 GB：HiBit Uninstaller 5.2、Kingsoft 2.9、Tencent 2.5、npm（全局包）1.3、QQEX 1.1、DingTalk 1.0、adspower_global 0.68、QQ 0.54、bilibili 0.48、youku-app 0.37、Code 0.32
- 点目录：.cache 4.9、.codex 3.7、.vscode(extensions) 2.9、.rustup 2.7、.cursor 0.86、.dsh 0.70、.cargo 0.60（registry 0.44）、.workbuddy-ai 1.3、.docker 0.24
- 其他：Documents 4.0（Tencent Files 0.87）、DeskBox 1.6、bin 1.0

## 推论
- 能安全回收的“纯垃圾”（Temp、缩略图、WER 等）合计只有约 1–2 GB；真正的大头是开发缓存、应用缓存/残留（HiBit 备份、updater 安装包、Package Cache），以及可以迁移到其他盘的大目录。
- 所以“分析 + 迁移”和“删垃圾”同样重要。
