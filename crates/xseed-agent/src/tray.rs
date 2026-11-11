//! Windows 托盘图标：双击或“打开 xseed”打开管理界面，“退出”结束程序。
//! 托盘需要在创建它的线程上运行 Win32 消息循环，所以服务在后台线程运行，主线程只负责托盘。

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, MessageBoxW, TranslateMessage, MB_ICONERROR, MB_OK, MSG};

pub fn run(url: &str, open_now: bool) -> anyhow::Result<()> {
    let menu = Menu::new();
    let open = MenuItem::new("打开 xseed", true, None);
    let quit = MenuItem::new("退出", true, None);
    menu.append_items(&[&open, &PredefinedMenuItem::separator(), &quit])?;
    let _tray = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("xseed 辅种小助手").with_icon(icon()).build()?;

    let (open_id, quit_id) = (open.id().clone(), quit.id().clone());
    let u = url.to_string();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        if e.id == open_id {
            open_browser(&u);
        } else if e.id == quit_id {
            std::process::exit(0);
        }
    }));
    let u = url.to_string();
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::DoubleClick { .. } = e {
            open_browser(&u);
        }
    }));
    if open_now {
        open_browser(url);
    }
    // SAFETY：标准 Win32 消息循环，MSG 为纯数据结构
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

fn open_browser(url: &str) {
    let _ = std::process::Command::new("explorer").arg(url).spawn();
}

/// 启动失败时弹窗提示（托盘程序没有控制台）。
pub fn fatal(text: &str) {
    let w = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (t, c) = (w(text), w("xseed"));
    // SAFETY：两个字符串都以 0 结尾，在调用期间有效
    unsafe {
        MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), MB_OK | MB_ICONERROR);
    }
}

/// 32×32 图标：星轨徽标（青柠色实心行星 + 外环），不依赖图片文件。
fn icon() -> Icon {
    const N: u32 = 32;
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    for y in 0..N {
        for x in 0..N {
            let (dx, dy) = (x as f32 - 15.5, y as f32 - 15.5);
            let d = (dx * dx + dy * dy).sqrt();
            let px = if d <= 8.0 {
                [0xC8, 0xF1, 0x69, 0xFF]
            } else if (12.0..=14.5).contains(&d) {
                [0xC8, 0xF1, 0x69, 0xB0]
            } else {
                [0, 0, 0, 0]
            };
            rgba.extend_from_slice(&px);
        }
    }
    Icon::from_rgba(rgba, N, N).expect("图标")
}
