// 界面上统一的数值显示格式。

// 大小按二进制单位显示，保留两位小数。
export function formatSize(bytes: number): string {
  const gb = 1024 * 1024 * 1024;
  const mb = 1024 * 1024;
  const kb = 1024;

  if (bytes >= gb) {
    return `${(bytes / gb).toFixed(2)} GB`;
  }
  if (bytes >= mb) {
    return `${(bytes / mb).toFixed(2)} MB`;
  }
  if (bytes >= kb) {
    return `${(bytes / kb).toFixed(2)} KB`;
  }
  return `${bytes} B`;
}

// 耗时统一用秒显示，分钟级的扫描也只要一位小数。
export function formatDuration(milliseconds: number): string {
  return `${(milliseconds / 1000).toFixed(1)} 秒`;
}

// 后端给的是 Unix 秒；0 表示时间不可用。
export function formatTime(seconds: number): string {
  if (seconds <= 0) {
    return "—";
  }
  return new Date(seconds * 1000).toLocaleString("zh-CN");
}
