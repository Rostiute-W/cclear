//! WinHTTP 的最小封装：只发一次 POST 请求。
//!
//! 这是全项目唯一的网络出口（见 AGENTS.md 规则 4），调用方只能是 `ai`。
//! 不跟随重定向：跟着跳转会把 Authorization 头原样带到另一台主机上。

use std::io;

use windows::Win32::Networking::WinHttp::{
    URL_COMPONENTS, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_OPEN_REQUEST_FLAGS, WINHTTP_OPTION_REDIRECT_POLICY,
    WINHTTP_OPTION_REDIRECT_POLICY_NEVER, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
    WinHttpCloseHandle, WinHttpConnect, WinHttpCrackUrl, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetOption, WinHttpSetTimeouts,
};
use windows::core::PCWSTR;

/// 一次请求的结果。状态码原样交给调用方判断。
pub struct HttpResponse {
    pub status: u32,
    pub body: Vec<u8>,
}

/// 每次读取响应的字节数。
const READ_CHUNK: usize = 8192;

/// 各阶段的超时（毫秒）：解析 15 秒、连接 15 秒、发送 30 秒、接收 120 秒。
const RESOLVE_TIMEOUT_MS: i32 = 15_000;
const CONNECT_TIMEOUT_MS: i32 = 15_000;
const SEND_TIMEOUT_MS: i32 = 30_000;
const RECEIVE_TIMEOUT_MS: i32 = 120_000;

/// 发一次 POST 请求，请求体按原样发送（调用方负责是 JSON）。
///
/// `limit` 是响应体的字节上限。超过上限时按 `InvalidData` 报告，调用方据此把
/// “响应过大”和真正的网络失败区分开。
pub fn post_json(
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    limit: usize,
) -> io::Result<HttpResponse> {
    let agent = wide_with_nul(&format!("CClear/{}", env!("CARGO_PKG_VERSION")));

    // SAFETY: 代理参数传空表示使用系统默认（自动发现代理）；UA 以 NUL 结尾且在调用期间存活。
    let session = Handle(unsafe {
        WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        )
    });
    if session.0.is_null() {
        return Err(io::Error::last_os_error());
    }

    // 解析出的主机名、路径等指针都指回 url_wide，后者在函数结束前一直存活。
    let url_wide: Vec<u16> = url.encode_utf16().collect();
    let mut parts = URL_COMPONENTS {
        dwStructSize: size_of::<URL_COMPONENTS>() as u32,
        // WinHttpCrackUrl 只在长度字段非 0 时才回写指针和长度；传 0 会让主机名和路径都是空的。
        dwHostNameLength: u32::MAX,
        dwUrlPathLength: u32::MAX,
        ..Default::default()
    };
    // SAFETY: 结构体已填好 dwStructSize 和需要回写的长度，URL 长度由切片给出（不含结尾 NUL）。
    unsafe { WinHttpCrackUrl(&url_wide, 0, &mut parts) }.map_err(io::Error::other)?;

    // SAFETY: 解析成功时这些指针和长度都落在 url_wide 内部的合法范围，长度按字符计。
    let (host, path) = unsafe {
        (
            wide_slice(parts.lpszHostName.0, parts.dwHostNameLength),
            wide_slice(parts.lpszUrlPath.0, parts.dwUrlPathLength),
        )
    };

    // 没有主机名就没法建连接，提前报错比让 WinHttpConnect 失败更容易看懂。
    if host.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "地址里没有主机名",
        ));
    }

    let mut host_nul = host.to_vec();
    host_nul.push(0);

    // 路径可以带查询串（本项目的地址校验会拒绝查询串，这里只保证不传空指针）。
    let mut path_nul = if path.is_empty() {
        vec![u16::from(b'/')]
    } else {
        path.to_vec()
    };
    path_nul.push(0);

    // SAFETY: 主机名以 NUL 结尾且在调用期间存活；端口取自解析结果。
    let connection =
        Handle(unsafe { WinHttpConnect(session.0, PCWSTR(host_nul.as_ptr()), parts.nPort, 0) });
    if connection.0.is_null() {
        return Err(io::Error::last_os_error());
    }

    // 地址已由 ai::endpoint_url 校验过，只可能是 https 或本机 http。
    let secure = url
        .get(..8)
        .is_some_and(|head| crate::win::equals_ignore_case(head, "https://"));
    let flags = if secure {
        WINHTTP_FLAG_SECURE
    } else {
        WINHTTP_OPEN_REQUEST_FLAGS(0)
    };

    let verb = wide_with_nul("POST");
    // SAFETY: 动词和路径都以 NUL 结尾且在调用期间存活；不接受额外类型头，所以类型数组传空。
    let request = Handle(unsafe {
        WinHttpOpenRequest(
            connection.0,
            PCWSTR(verb.as_ptr()),
            PCWSTR(path_nul.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            flags,
        )
    });
    if request.0.is_null() {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: 请求句柄有效，超时只约束这一次请求。
    unsafe {
        WinHttpSetTimeouts(
            request.0,
            RESOLVE_TIMEOUT_MS,
            CONNECT_TIMEOUT_MS,
            SEND_TIMEOUT_MS,
            RECEIVE_TIMEOUT_MS,
        )
    }
    .map_err(io::Error::other)?;

    let redirect_policy = WINHTTP_OPTION_REDIRECT_POLICY_NEVER.to_ne_bytes();
    // SAFETY: 策略值是 4 字节整数，缓冲区在调用期间存活。
    unsafe {
        WinHttpSetOption(
            Some(request.0 as *const core::ffi::c_void),
            WINHTTP_OPTION_REDIRECT_POLICY,
            Some(&redirect_policy),
        )
    }
    .map_err(io::Error::other)?;

    let mut header_text = String::new();
    for (name, value) in headers {
        header_text.push_str(name);
        header_text.push_str(": ");
        header_text.push_str(value);
        header_text.push_str("\r\n");
    }
    let header_wide: Vec<u16> = header_text.encode_utf16().collect();

    // SAFETY: 请求体指针与长度一致，头是按字符计数切片的宽字符串；调用期间都有效。
    unsafe {
        WinHttpSendRequest(
            request.0,
            if header_wide.is_empty() {
                None
            } else {
                Some(&header_wide)
            },
            Some(body.as_ptr() as *const core::ffi::c_void),
            body.len() as u32,
            body.len() as u32,
            0,
        )
    }
    .map_err(io::Error::other)?;

    // SAFETY: 请求句柄有效，保留参数按要求传空。
    unsafe { WinHttpReceiveResponse(request.0, std::ptr::null_mut()) }.map_err(io::Error::other)?;

    let mut status = 0u32;
    let mut status_length = size_of::<u32>() as u32;
    let mut status_index = 0u32;
    // SAFETY: 缓冲区就是一个 u32，长度按字节给出；查询形式为数字，不走字符串转换。
    unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut core::ffi::c_void),
            &mut status_length,
            &mut status_index,
        )
    }
    .map_err(io::Error::other)?;

    let mut read_body = Vec::new();
    let mut buffer = [0u8; READ_CHUNK];
    loop {
        let mut read = 0u32;
        // SAFETY: 缓冲区是本函数内的数组，长度与数组一致；实际读到的字节数由系统回写。
        unsafe {
            WinHttpReadData(
                request.0,
                buffer.as_mut_ptr() as *mut core::ffi::c_void,
                READ_CHUNK as u32,
                &mut read,
            )
        }
        .map_err(io::Error::other)?;

        if read == 0 {
            break;
        }
        if read_body.len() + read as usize > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "响应超过允许的大小",
            ));
        }
        read_body.extend_from_slice(&buffer[..read as usize]);
    }

    Ok(HttpResponse {
        status,
        body: read_body,
    })
}

/// WinHTTP 句柄。析构时关闭，保证每条出错分支都不漏句柄。
struct Handle(*mut core::ffi::c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        // 空句柄说明创建失败，没有可关闭的东西。
        if self.0.is_null() {
            return;
        }
        // SAFETY: 句柄由 WinHTTP 创建，全程只在这里关闭一次。
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// 转成以 NUL 结尾的宽字符串，供要求 `LPCWSTR` 的参数使用。
fn wide_with_nul(text: &str) -> Vec<u16> {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    wide
}

/// 从 WinHTTP 回写的指针和长度（按字符计）取宽字符串。
///
/// # Safety
/// 调用方要保证指针指向至少 `length` 个 `u16`，且这些字符在返回值的生命周期内有效。
unsafe fn wide_slice<'a>(pointer: *const u16, length: u32) -> &'a [u16] {
    if pointer.is_null() || length == 0 {
        return &[];
    }
    // SAFETY: 由调用方保证指针和长度有效。
    unsafe { std::slice::from_raw_parts(pointer, length as usize) }
}
