//! screen-mirror (GUI): mirrors one monitor onto another with Windows Graphics Capture + D3D11.
//! Control window with source/destination pickers. The mirror opens as a floating window
//! that can be made fullscreen with F11 (Esc leaves fullscreen); stop it with "Stop".

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::{
    core::*,
    Foundation::TypedEventHandler,
    Graphics::{
        Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession},
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
    },
    Win32::{
        Devices::Display::*,
        Foundation::*,
        Graphics::{
            Direct3D::*,
            Direct3D11::*,
            Dxgi::{Common::*, *},
            Gdi::*,
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            WinRT::{
                Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
                Graphics::Capture::IGraphicsCaptureItemInterop,
                RoInitialize, RO_INIT_MULTITHREADED,
            },
        },
        UI::{HiDpi::*, Input::KeyboardAndMouse::EnableWindow, WindowsAndMessaging::*},
    },
};

const PIXEL_FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;

// Control IDs
const ID_START: i32 = 1;
const ID_REFRESH: i32 = 2;
const ID_VSYNC: i32 = 3;
const ID_SRC: i32 = 4;

// "Open monitor" dialog control IDs (IDOK/IDCANCEL are 1 and 2)
const ID_DLG_LIST: i32 = 101;
const ID_DLG_FULL: i32 = 102;
const ID_DLG_OPEN: i32 = 103;
const ID_DLG_CANCEL: i32 = 104;

// Control messages (literals so we do not depend on constant types)
const M_CB_ADDSTRING: u32 = 0x0143;
const M_CB_GETCURSEL: u32 = 0x0147;
const M_CB_SETCURSEL: u32 = 0x014E;
const M_CB_RESETCONTENT: u32 = 0x014B;
const M_BM_GETCHECK: u32 = 0x00F0;
const M_BM_SETCHECK: u32 = 0x00F1;
const M_WM_SETFONT: u32 = 0x0030;

const WM_MIRROR_CLOSED: u32 = WM_APP + 1;

// Virtual-key codes (Win32::UI::Input::KeyboardAndMouse is not a dependency)
const VK_F11_KEY: u16 = 0x7A;
const VK_ESCAPE_KEY: u16 = 0x1B;

static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

// ------------------------------------------------------------------ helpers

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn set_text(h: HWND, s: &str) {
    let t = wide(s);
    let _ = SetWindowTextW(h, PCWSTR(t.as_ptr()));
}

struct SendDev(IDirect3DDevice);
unsafe impl Send for SendDev {}
impl SendDev {
    fn get(&self) -> &IDirect3DDevice {
        &self.0
    }
}

#[derive(Clone)]
struct Mon {
    h: HMONITOR,
    rect: RECT,
    primary: bool,
    gdi: String,  // e.g. \\.\DISPLAY3
    name: String, // monitor name from its EDID
}

fn utf16_to_string(a: &[u16]) -> String {
    let n = a.iter().position(|&c| c == 0).unwrap_or(a.len());
    String::from_utf16_lossy(&a[..n])
}

/// Returns (GDI name, friendly name) pairs for the active monitors.
fn friendly_names() -> Vec<(String, String)> {
    let mut out = Vec::new();
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != ERROR_SUCCESS {
            return out;
        }
        let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> =
            (0..np).map(|_| std::mem::zeroed()).collect();
        let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> =
            (0..nm).map(|_| std::mem::zeroed()).collect();
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut np,
            paths.as_mut_ptr(),
            &mut nm,
            modes.as_mut_ptr(),
            None,
        ) != ERROR_SUCCESS
        {
            return out;
        }
        for p in paths.iter().take(np as usize) {
            let mut sn: DISPLAYCONFIG_SOURCE_DEVICE_NAME = std::mem::zeroed();
            sn.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            sn.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            sn.header.adapterId = p.sourceInfo.adapterId;
            sn.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut sn.header) != 0 {
                continue;
            }

            let mut tn: DISPLAYCONFIG_TARGET_DEVICE_NAME = std::mem::zeroed();
            tn.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            tn.header.size = size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            tn.header.adapterId = p.targetInfo.adapterId;
            tn.header.id = p.targetInfo.id;
            if DisplayConfigGetDeviceInfo(&mut tn.header) != 0 {
                continue;
            }

            out.push((
                utf16_to_string(&sn.viewGdiDeviceName),
                utf16_to_string(&tn.monitorFriendlyDeviceName),
            ));
        }
    }
    out
}

unsafe extern "system" fn enum_cb(h: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
    let v = &mut *(lp.0 as *mut Vec<Mon>);
    let mut mi: MONITORINFOEXW = std::mem::zeroed();
    mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    if GetMonitorInfoW(h, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool() {
        v.push(Mon {
            h,
            rect: mi.monitorInfo.rcMonitor,
            primary: mi.monitorInfo.dwFlags & 1 != 0,
            gdi: utf16_to_string(&mi.szDevice),
            name: String::new(),
        });
    }
    BOOL(1)
}

fn monitors() -> Vec<Mon> {
    let mut v: Vec<Mon> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(enum_cb), LPARAM(&mut v as *mut _ as isize));
    }
    let names = friendly_names();
    for m in v.iter_mut() {
        if let Some((_, n)) = names.iter().find(|(g, _)| *g == m.gdi) {
            m.name = n.clone();
        }
        if m.name.trim().is_empty() {
            m.name = "Unnamed monitor".to_string();
        }
    }
    v
}

/// Display label: "LG TV SSCR2 - 1920x1080 [primary]".
fn mon_label(m: &Mon) -> String {
    format!(
        "{} - {}x{}{}",
        m.name,
        m.rect.right - m.rect.left,
        m.rect.bottom - m.rect.top,
        if m.primary { " [primary]" } else { "" }
    )
}

// ------------------------------------------------------------------ mirror engine

struct Mirror {
    hwnd: HWND,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    token: windows::Foundation::EventRegistrationToken,
    src_idx: usize,
    dst_idx: usize,
    fullscreen: bool,
    saved_style: isize,
    saved_rect: RECT,
}

/// Floating window inside the destination monitor: 70% wide, 16:9, centered.
fn float_rect(dst: &RECT) -> RECT {
    let mw = (dst.right - dst.left).max(1);
    let mh = (dst.bottom - dst.top).max(1);
    let mut w = mw * 7 / 10;
    let mut h = w * 9 / 16;
    if h > mh * 8 / 10 {
        h = mh * 8 / 10;
        w = h * 16 / 9;
    }
    let x = dst.left + (mw - w) / 2;
    let y = dst.top + (mh - h) / 2;
    RECT { left: x, top: y, right: x + w, bottom: y + h }
}

/// Toggles the mirror window between floating and borderless fullscreen (F11).
unsafe fn toggle_fullscreen(h: HWND) {
    let go_full = MIRROR.with(|c| {
        let mut b = c.borrow_mut();
        match b.as_mut() {
            Some(m) => {
                let was_full = m.fullscreen;
                if !was_full {
                    let mut r = RECT::default();
                    let _ = GetWindowRect(h, &mut r);
                    m.saved_rect = r;
                    m.saved_style = GetWindowLongPtrW(h, GWL_STYLE);
                }
                m.fullscreen = !was_full;
                m.fullscreen
            }
            None => false,
        }
    });

    if go_full {
        let mon = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = size_of::<MONITORINFO>() as u32;
        let _ = GetMonitorInfoW(mon, &mut mi);
        let r = mi.rcMonitor;
        SetWindowLongPtrW(h, GWL_STYLE, (WS_POPUP | WS_VISIBLE).0 as isize);
        let _ = SetWindowPos(
            h,
            None,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            SWP_NOZORDER | SWP_FRAMECHANGED,
        );
    } else {
        let (style, r) = MIRROR.with(|c| {
            let b = c.borrow();
            let m = b.as_ref().unwrap();
            (m.saved_style, m.saved_rect)
        });
        SetWindowLongPtrW(h, GWL_STYLE, style);
        let _ = SetWindowPos(
            h,
            None,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            SWP_NOZORDER | SWP_FRAMECHANGED,
        );
    }
}

impl Mirror {
    fn start(src: &Mon, dst: &Mon, vsync: bool) -> Result<Mirror> {
        let sync: u32 = if vsync { 1 } else { 0 };

        // Floating window inside the destination monitor; F11 toggles fullscreen.
        let hwnd = unsafe {
            let hinst: HINSTANCE = GetModuleHandleW(None)?.into();
            let r = float_rect(&dst.rect);
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("ScreenMirrorWnd"),
                w!("Screen Mirror"),
                WS_OVERLAPPEDWINDOW,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                None,
                None,
                hinst,
                None,
            )?;
            hwnd
        };

        let result = Self::start_inner(src, hwnd, sync);
        match result {
            Ok((pool, session, token)) => {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                }
                Ok(Mirror {
                    hwnd,
                    pool,
                    session,
                    token,
                    src_idx: 0,
                    dst_idx: 0,
                    fullscreen: false,
                    saved_style: 0,
                    saved_rect: RECT::default(),
                })
            }
            Err(e) => {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                Err(e)
            }
        }
    }

    fn start_inner(
        src: &Mon,
        hwnd: HWND,
        sync: u32,
    ) -> Result<(
        Direct3D11CaptureFramePool,
        GraphicsCaptureSession,
        windows::Foundation::EventRegistrationToken,
    )> {
        // D3D11 device
        let mut device: Option<ID3D11Device> = None;
        let mut ctx: Option<ID3D11DeviceContext> = None;
        unsafe {
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            )?;
        }
        let device = device.unwrap();
        let ctx = ctx.unwrap();
        let mt: ID3D11Multithread = ctx.cast()?;
        unsafe {
            let _ = mt.SetMultithreadProtected(true);
        }
        let dxgi_dev: IDXGIDevice = device.cast()?;
        let winrt_dev: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_dev)?.cast()? };

        // Capture the source monitor
        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(src.h)? };
        let size = item.Size()?;

        // Swap chain (STRETCH: DWM scales it to the window)
        let factory2: IDXGIFactory2 = unsafe { dxgi_dev.GetAdapter()?.GetParent()? };
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: size.Width as u32,
            Height: size.Height as u32,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: FALSE,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: 0,
        };
        let swap: IDXGISwapChain1 =
            unsafe { factory2.CreateSwapChainForHwnd(&device, hwnd, &desc, None, None)? };
        unsafe {
            let _ = factory2.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
        }

        // Frame pool + handler
        let pool =
            Direct3D11CaptureFramePool::CreateFreeThreaded(&winrt_dev, PIXEL_FORMAT, 2, size)?;

        let pool_h = pool.clone();
        let dev_h = SendDev(winrt_dev.clone());
        let swap_h = swap.clone();
        let ctx_h = ctx.clone();
        let mut pool_size = (size.Width as u32, size.Height as u32);
        let mut swap_size = pool_size;

        let token = pool.FrameArrived(&TypedEventHandler::<
            Direct3D11CaptureFramePool,
            IInspectable,
        >::new(move |_, _| {
            let frame = match pool_h.TryGetNextFrame() {
                Ok(f) => f,
                Err(_) => return Ok(()),
            };
            // If the source resolution changes, recreate the pool and skip this frame.
            let cs = frame.ContentSize()?;
            let cs_u = (cs.Width as u32, cs.Height as u32);
            if cs_u != pool_size {
                pool_h.Recreate(dev_h.get(), PIXEL_FORMAT, 2, cs)?;
                pool_size = cs_u;
                return Ok(());
            }
            if swap_size != pool_size {
                unsafe {
                    swap_h.ResizeBuffers(
                        0,
                        pool_size.0,
                        pool_size.1,
                        DXGI_FORMAT_UNKNOWN,
                        DXGI_SWAP_CHAIN_FLAG(0),
                    )?;
                }
                swap_size = pool_size;
            }
            let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
            let tex: ID3D11Texture2D = unsafe { access.GetInterface()? };
            let back: ID3D11Texture2D = unsafe { swap_h.GetBuffer(0)? };
            unsafe {
                ctx_h.CopyResource(&back, &tex);
            }
            drop(back);
            unsafe {
                let _ = swap_h.Present(sync, DXGI_PRESENT(0));
            }
            Ok(())
        }))?;

        let session = pool.CreateCaptureSession(&item)?;
        let _ = session.SetIsCursorCaptureEnabled(true);
        // Windows 11 only; ignored on Windows 10 (yellow border).
        let _ = session.SetIsBorderRequired(false);
        session.StartCapture()?;

        Ok((pool, session, token))
    }
}

impl Drop for Mirror {
    fn drop(&mut self) {
        let _ = self.pool.RemoveFrameArrived(self.token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
        unsafe {
            if IsWindow(self.hwnd).as_bool() {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

// ------------------------------------------------------------------ GUI state

#[derive(Clone, Copy)]
struct Ui {
    panel: HWND,
    cb_src: HWND,
    chk_vsync: HWND,
    btn_start: HWND,
    status: HWND,
}

/// State of the "Open monitor" dialog while it is on screen.
#[derive(Clone)]
struct OpenDlg {
    hwnd: HWND,
    list: HWND,
    full: HWND,
    src: usize,
    dst: Vec<usize>, // monitor index for each row of the list
}

thread_local! {
    static UI: RefCell<Option<Ui>> = RefCell::new(None);
    static MONS: RefCell<Vec<Mon>> = RefCell::new(Vec::new());
    static MIRROR: RefCell<Option<Mirror>> = RefCell::new(None);
    static OPEN_DLG: RefCell<Option<OpenDlg>> = RefCell::new(None);
    // Last destination (GDI name) and fullscreen choice, remembered per session.
    static LAST_OPEN: RefCell<Option<(String, bool)>> = RefCell::new(None);
}

unsafe fn make_ctl(
    parent: HWND,
    hinst: HINSTANCE,
    class: PCWSTR,
    text: &str,
    style: u32,
    (x, y, w, h): (i32, i32, i32, i32),
    id: i32,
) -> HWND {
    let t = wide(text);
    let hw = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        class,
        PCWSTR(t.as_ptr()),
        WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
        x,
        y,
        w,
        h,
        parent,
        HMENU(id as usize as *mut c_void),
        hinst,
        None,
    )
    .unwrap_or_default();
    let font = GetStockObject(DEFAULT_GUI_FONT);
    SendMessageW(hw, M_WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
    hw
}

unsafe fn build_ui(parent: HWND) {
    let hinst: HINSTANCE = GetModuleHandleW(None).unwrap().into();
    let tab = WS_TABSTOP.0;

    let panel = make_ctl(parent, hinst, w!("ScreenMirrorPanel"), "", 0, (16, 14, 328, 160), 0);
    make_ctl(parent, hinst, w!("STATIC"), "Left click a monitor to choose the one to mirror.", 0, (16, 178, 328, 18), 0);

    make_ctl(parent, hinst, w!("STATIC"), "Monitor to mirror:", 0, (16, 206, 328, 18), 0);
    let cb_src = make_ctl(parent, hinst, w!("COMBOBOX"), "", 0x3 | tab, (16, 226, 328, 200), ID_SRC);
    let chk_vsync = make_ctl(
        parent,
        hinst,
        w!("BUTTON"),
        "VSync (no tearing, a little more latency)",
        0x3 | tab, // BS_AUTOCHECKBOX
        (16, 262, 328, 22),
        ID_VSYNC,
    );
    let btn_start = make_ctl(parent, hinst, w!("BUTTON"), "Open monitor...", 0x1 | tab, (16, 296, 160, 30), ID_START);
    make_ctl(parent, hinst, w!("BUTTON"), "Refresh monitors", tab, (184, 296, 160, 30), ID_REFRESH);
    let status = make_ctl(parent, hinst, w!("STATIC"), "Ready.", 0, (16, 334, 328, 42), 0);

    UI.with(|u| {
        *u.borrow_mut() = Some(Ui { panel, cb_src, chk_vsync, btn_start, status });
    });
    refresh_monitors();
}

unsafe fn fill_combo(cb: HWND, mons: &[Mon], sel: i32) {
    SendMessageW(cb, M_CB_RESETCONTENT, WPARAM(0), LPARAM(0));
    for (i, m) in mons.iter().enumerate() {
        let _ = i;
        let t = wide(&mon_label(m));
        SendMessageW(cb, M_CB_ADDSTRING, WPARAM(0), LPARAM(t.as_ptr() as isize));
    }
    SendMessageW(cb, M_CB_SETCURSEL, WPARAM(sel.max(0) as usize), LPARAM(0));
}

unsafe fn refresh_monitors() {
    let ui = match UI.with(|u| *u.borrow()) {
        Some(u) => u,
        None => return,
    };
    let old_s = SendMessageW(ui.cb_src, M_CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as i32;

    let mons = monitors();
    let n = mons.len() as i32;
    let prim = mons.iter().position(|m| m.primary).unwrap_or(0) as i32;
    let s = if old_s >= 0 && old_s < n { old_s } else { prim };

    fill_combo(ui.cb_src, &mons, s);
    MONS.with(|m| *m.borrow_mut() = mons);
    let _ = InvalidateRect(ui.panel, None, FALSE);
}

unsafe fn stop() {
    let m = MIRROR.with(|c| c.borrow_mut().take());
    let was_running = m.is_some();
    drop(m);
    if let Some(ui) = UI.with(|u| *u.borrow()) {
        set_text(ui.btn_start, "Open monitor...");
        if was_running {
            set_text(ui.status, "Stopped.");
        }
        let _ = InvalidateRect(ui.panel, None, FALSE);
    }
}

/// Opens the dialog that asks where the mirror will be shown and whether it
/// starts fullscreen. With the mirror already running, the same button stops it.
unsafe fn toggle() {
    if MIRROR.with(|c| c.borrow().is_some()) {
        stop();
        return;
    }
    open_dialog();
}

unsafe fn open_dialog() {
    if OPEN_DLG.with(|d| d.borrow().is_some()) {
        return;
    }
    let ui = match UI.with(|u| *u.borrow()) {
        Some(u) => u,
        None => return,
    };
    let mons = MONS.with(|m| m.borrow().clone());
    let s = SendMessageW(ui.cb_src, M_CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as i32;
    if s < 0 || s as usize >= mons.len() {
        set_text(ui.status, "Select a monitor to mirror first.");
        return;
    }
    let src = s as usize;
    let dst: Vec<usize> = (0..mons.len()).filter(|&i| i != src).collect();
    if dst.is_empty() {
        set_text(ui.status, "Only one monitor is connected, nothing to mirror to.");
        return;
    }

    let hinst: HINSTANCE = GetModuleHandleW(None).unwrap().into();
    let tab = WS_TABSTOP.0;

    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
    let mut r = RECT { left: 0, top: 0, right: 380, bottom: 182 };
    let _ = AdjustWindowRect(&mut r, style, FALSE);
    let (w, h) = (r.right - r.left, r.bottom - r.top);

    let main = HWND(MAIN_HWND.load(Ordering::Relaxed) as *mut c_void);
    let mut pr = RECT::default();
    let _ = GetWindowRect(main, &mut pr);
    let x = pr.left + ((pr.right - pr.left) - w) / 2;
    let y = pr.top + ((pr.bottom - pr.top) - h) / 2;

    let dlg = match CreateWindowExW(
        WS_EX_DLGMODALFRAME | WS_EX_CONTROLPARENT,
        w!("ScreenMirrorDlg"),
        w!("Open monitor"),
        style,
        x,
        y,
        w,
        h,
        main,
        None,
        hinst,
        None,
    ) {
        Ok(h) => h,
        Err(_) => return,
    };

    make_ctl(dlg, hinst, w!("STATIC"), &format!("Mirror: {}", mons[src].name), 0, (16, 14, 348, 18), 0);
    make_ctl(dlg, hinst, w!("STATIC"), "Show on:", 0, (16, 44, 348, 18), 0);
    let list = make_ctl(dlg, hinst, w!("COMBOBOX"), "", 0x3 | tab, (16, 64, 348, 200), ID_DLG_LIST);
    let full = make_ctl(dlg, hinst, w!("BUTTON"), "Open fullscreen", 0x3 | tab, (16, 98, 348, 22), ID_DLG_FULL);
    make_ctl(dlg, hinst, w!("BUTTON"), "Open", 0x1 | tab, (16, 130, 170, 30), ID_DLG_OPEN);
    make_ctl(dlg, hinst, w!("BUTTON"), "Cancel", tab, (194, 130, 170, 30), ID_DLG_CANCEL);

    for &i in &dst {
        let t = wide(&mon_label(&mons[i]));
        SendMessageW(list, M_CB_ADDSTRING, WPARAM(0), LPARAM(t.as_ptr() as isize));
    }
    // Preselect the destination used last time, or the first one.
    let last = LAST_OPEN.with(|l| l.borrow().clone());
    let mut sel = 0;
    if let Some((gdi, was_full)) = last {
        if let Some(p) = dst.iter().position(|&i| mons[i].gdi == gdi) {
            sel = p;
        }
        SendMessageW(full, M_BM_SETCHECK, WPARAM(if was_full { 1 } else { 0 }), LPARAM(0));
    }
    SendMessageW(list, M_CB_SETCURSEL, WPARAM(sel), LPARAM(0));

    OPEN_DLG.with(|d| {
        *d.borrow_mut() = Some(OpenDlg { hwnd: dlg, list, full, src, dst });
    });
    let _ = EnableWindow(main, false);
    let _ = ShowWindow(dlg, SW_SHOW);
    let _ = SetForegroundWindow(dlg);
}

unsafe fn close_open_dialog() {
    let h = OPEN_DLG.with(|d| d.borrow().as_ref().map(|x| x.hwnd));
    if let Some(h) = h {
        let _ = DestroyWindow(h);
    }
}

unsafe fn open_selected() {
    let ui = match UI.with(|u| *u.borrow()) {
        Some(u) => u,
        None => return,
    };
    let (src, dst, fullscreen) = {
        let d = match OPEN_DLG.with(|d| d.borrow().clone()) {
            Some(d) => d,
            None => return,
        };
        let sel = SendMessageW(d.list, M_CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as i32;
        if sel < 0 || sel as usize >= d.dst.len() {
            return;
        }
        let dst = d.dst[sel as usize];
        let full = SendMessageW(d.full, M_BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == 1;
        let gdi = MONS.with(|m| m.borrow()[dst].gdi.clone());
        LAST_OPEN.with(|l| *l.borrow_mut() = Some((gdi, full)));
        (d.src, dst, full)
    };
    close_open_dialog();
    start_mirror(ui, src, dst, fullscreen);
}

unsafe fn start_mirror(ui: Ui, src: usize, dst: usize, fullscreen: bool) {
    let mons = MONS.with(|m| m.borrow().clone());
    if src >= mons.len() || dst >= mons.len() {
        return;
    }
    let vsync = SendMessageW(ui.chk_vsync, M_BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == 1;

    match Mirror::start(&mons[src], &mons[dst], vsync) {
        Ok(mut m) => {
            m.src_idx = src;
            m.dst_idx = dst;
            let hwnd = m.hwnd;
            MIRROR.with(|c| *c.borrow_mut() = Some(m));
            set_text(ui.btn_start, "Stop");
            set_text(
                ui.status,
                &format!(
                    "Mirroring {} on {}{}.\r\nPress F11 in the mirror window for fullscreen.",
                    mons[src].name,
                    mons[dst].name,
                    if fullscreen { " (fullscreen)" } else { "" }
                ),
            );
            if fullscreen {
                toggle_fullscreen(hwnd);
            }
            let _ = InvalidateRect(ui.panel, None, FALSE);
        }
        Err(e) => {
            set_text(ui.status, &format!("Error: {}", e.message()));
        }
    }
}

// ------------------------------------------------------------------ window procs

unsafe extern "system" fn ctl_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match m {
        WM_CREATE => {
            MAIN_HWND.store(h.0 as isize, Ordering::Relaxed);
            build_ui(h);
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (w.0 & 0xFFFF) as i32;
            let code = ((w.0 >> 16) & 0xFFFF) as u32;
            match id {
                ID_START => toggle(),
                ID_REFRESH => refresh_monitors(),
                // CBN_SELCHANGE = 1
                ID_SRC if code == 1 => {
                    if let Some(ui) = UI.with(|u| *u.borrow()) {
                        let _ = InvalidateRect(ui.panel, None, FALSE);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_MIRROR_CLOSED => {
            let cur = MIRROR.with(|c| c.borrow().as_ref().map(|m| m.hwnd.0 as isize));
            if cur == Some(l.0) {
                stop();
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            stop();
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(h, m, w, l),
    }
}

unsafe extern "system" fn dialog_proc(h: HWND, m: u32, w: WPARAM, _l: LPARAM) -> LRESULT {
    match m {
        WM_COMMAND => {
            // IsDialogMessageW sends IDOK (1) for Enter and IDCANCEL (2) for Esc.
            match (w.0 & 0xFFFF) as i32 {
                ID_DLG_OPEN | 1 => open_selected(),
                ID_DLG_CANCEL | 2 => close_open_dialog(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            close_open_dialog();
            LRESULT(0)
        }
        WM_DESTROY => {
            OPEN_DLG.with(|d| {
                d.borrow_mut().take();
            });
            let main = HWND(MAIN_HWND.load(Ordering::Relaxed) as *mut c_void);
            let _ = EnableWindow(main, true);
            LRESULT(0)
        }
        _ => DefWindowProcW(h, m, w, _l),
    }
}

unsafe extern "system" fn mirror_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match m {
        WM_DESTROY => {
            let main = HWND(MAIN_HWND.load(Ordering::Relaxed) as *mut c_void);
            let _ = PostMessageW(main, WM_MIRROR_CLOSED, WPARAM(0), LPARAM(h.0 as isize));
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = w.0 as u16;
            if vk == VK_F11_KEY {
                toggle_fullscreen(h);
                LRESULT(0)
            } else if vk == VK_ESCAPE_KEY {
                let full = MIRROR.with(|c| c.borrow().as_ref().map(|m| m.fullscreen).unwrap_or(false));
                if full {
                    toggle_fullscreen(h);
                }
                LRESULT(0)
            } else {
                DefWindowProcW(h, m, w, l)
            }
        }
        // The cursor is only hidden in fullscreen; in floating mode it is needed
        // to move and resize the window.
        WM_SETCURSOR => {
            let full = MIRROR.with(|c| c.borrow().as_ref().map(|m| m.fullscreen).unwrap_or(false));
            if full {
                SetCursor(None);
                LRESULT(1)
            } else {
                DefWindowProcW(h, m, w, l)
            }
        }
        _ => DefWindowProcW(h, m, w, l),
    }
}

// ------------------------------------------------------------------ monitor panel

/// Scales the real monitor arrangement into the (w x h) area of the panel.
fn layout(mons: &[Mon], w: i32, h: i32) -> Vec<RECT> {
    if mons.is_empty() {
        return Vec::new();
    }
    let minx = mons.iter().map(|m| m.rect.left).min().unwrap();
    let miny = mons.iter().map(|m| m.rect.top).min().unwrap();
    let maxx = mons.iter().map(|m| m.rect.right).max().unwrap();
    let maxy = mons.iter().map(|m| m.rect.bottom).max().unwrap();
    let bw = (maxx - minx).max(1) as f64;
    let bh = (maxy - miny).max(1) as f64;
    let pad = 10.0;
    let sc = ((w as f64 - 2.0 * pad) / bw).min((h as f64 - 2.0 * pad) / bh);
    let ox = (w as f64 - bw * sc) / 2.0;
    let oy = (h as f64 - bh * sc) / 2.0;
    mons.iter()
        .map(|m| RECT {
            left: (ox + (m.rect.left - minx) as f64 * sc) as i32,
            top: (oy + (m.rect.top - miny) as f64 * sc) as i32,
            right: (ox + (m.rect.right - minx) as f64 * sc) as i32,
            bottom: (oy + (m.rect.bottom - miny) as f64 * sc) as i32,
        })
        .collect()
}

unsafe fn paint_panel(h: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(h, &mut ps);

    let mut rc = RECT::default();
    let _ = GetClientRect(h, &mut rc);

    // Colors in 0x00BBGGRR format
    let bg = CreateSolidBrush(COLORREF(0x0028_2420));
    FillRect(hdc, &rc, bg);
    let _ = DeleteObject(HGDIOBJ(bg.0));

    let mons = MONS.with(|m| m.borrow().clone());
    let rects = layout(&mons, rc.right, rc.bottom);

    // Source: the running mirror, otherwise the drop-down selection.
    let src_idx = MIRROR.with(|c| c.borrow().as_ref().map(|m| m.src_idx as i32)).unwrap_or_else(|| {
        UI.with(|u| match *u.borrow() {
            Some(ui) => SendMessageW(ui.cb_src, M_CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as i32,
            None => -1,
        })
    });

    // The mirror is shown wherever its window is right now: it can be dragged.
    let dst_idx = MIRROR.with(|c| {
        c.borrow().as_ref().and_then(|m| {
            let mon = MonitorFromWindow(m.hwnd, MONITOR_DEFAULTTONEAREST);
            MONS.with(|ms| ms.borrow().iter().position(|x| x.h == mon).map(|p| p as i32))
        })
    });

    let font = GetStockObject(DEFAULT_GUI_FONT);
    let old_font = SelectObject(hdc, font);
    SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, COLORREF(0x00FF_FFFF));

    let border = CreateSolidBrush(COLORREF(0x00C8_C8C8));
    for (i, m) in mons.iter().enumerate() {
        let mut r = rects[i];
        r.left += 1;
        r.top += 1;
        r.right -= 1;
        r.bottom -= 1;

        let i = i as i32;
        let (color, tag) = if i == src_idx && Some(i) == dst_idx {
            (0x0000_80F0, "\r\n[SOURCE and MIRROR]")
        } else if i == src_idx {
            (0x00F6_7834, "\r\n[SOURCE]")
        } else if Some(i) == dst_idx {
            (0x0043_A02E, "\r\n[MIRROR]")
        } else {
            (0x0050_4A44, "")
        };
        let fill = CreateSolidBrush(COLORREF(color));
        FillRect(hdc, &r, fill);
        let _ = DeleteObject(HGDIOBJ(fill.0));
        FrameRect(hdc, &r, border);

        let text = format!(
            "{}\r\n{}x{}{}{}",
            m.name,
            m.rect.right - m.rect.left,
            m.rect.bottom - m.rect.top,
            if m.primary { " (primary)" } else { "" },
            tag
        );
        let mut buf: Vec<u16> = text.encode_utf16().collect();
        let mut inner = RECT {
            left: r.left + 4,
            top: r.top + 4,
            right: r.right - 4,
            bottom: r.bottom - 4,
        };
        let fmt = DT_CENTER | DT_WORDBREAK | DT_NOPREFIX;
        // Vertical centering: measure the text height first.
        let mut calc = inner;
        DrawTextW(hdc, &mut buf, &mut calc, fmt | DT_CALCRECT);
        let free = (inner.bottom - inner.top) - (calc.bottom - calc.top);
        if free > 0 {
            inner.top += free / 2;
        }
        DrawTextW(hdc, &mut buf, &mut inner, fmt);
    }
    let _ = DeleteObject(HGDIOBJ(border.0));
    SelectObject(hdc, old_font);

    let _ = EndPaint(h, &ps);
}

unsafe fn panel_click(h: HWND, l: LPARAM) {
    if MIRROR.with(|c| c.borrow().is_some()) {
        return; // the selection cannot change while mirroring
    }
    let ui = match UI.with(|u| *u.borrow()) {
        Some(u) => u,
        None => return,
    };
    let x = (l.0 & 0xFFFF) as u16 as i16 as i32;
    let y = ((l.0 >> 16) & 0xFFFF) as u16 as i16 as i32;

    let mut rc = RECT::default();
    let _ = GetClientRect(h, &mut rc);
    let mons = MONS.with(|m| m.borrow().clone());
    let rects = layout(&mons, rc.right, rc.bottom);

    if let Some(i) = rects
        .iter()
        .position(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
    {
        SendMessageW(ui.cb_src, M_CB_SETCURSEL, WPARAM(i), LPARAM(0));
        let _ = InvalidateRect(h, None, FALSE);
    }
}

unsafe extern "system" fn panel_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match m {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint_panel(h);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            panel_click(h, l);
            LRESULT(0)
        }
        _ => DefWindowProcW(h, m, w, l),
    }
}

// ------------------------------------------------------------------ main

fn main() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        RoInitialize(RO_INIT_MULTITHREADED)?;

        let hinst: HINSTANCE = GetModuleHandleW(None)?.into();

        // Icon embedded by build.rs (resource with ID 1)
        let icon = LoadIconW(hinst, PCWSTR(1usize as *const u16)).unwrap_or_default();

        // Control window class
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(ctl_proc),
            hInstance: hinst,
            hIcon: icon,
            hIconSm: icon,
            lpszClassName: w!("ScreenMirrorCtl"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as usize as *mut c_void),
            ..Default::default()
        };
        RegisterClassExW(&wc);

        // Mirror window class (black background)
        let black = GetStockObject(BLACK_BRUSH);
        let wc2 = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(mirror_proc),
            hInstance: hinst,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: w!("ScreenMirrorWnd"),
            hbrBackground: HBRUSH(black.0),
            ..Default::default()
        };
        RegisterClassExW(&wc2);

        // Class of the panel that draws the monitors
        let wc3 = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(panel_proc),
            hInstance: hinst,
            lpszClassName: w!("ScreenMirrorPanel"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassExW(&wc3);

        // Class of the "Open monitor" dialog
        let wc4 = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(dialog_proc),
            hInstance: hinst,
            hIcon: icon,
            hIconSm: icon,
            lpszClassName: w!("ScreenMirrorDlg"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as usize as *mut c_void),
            ..Default::default()
        };
        RegisterClassExW(&wc4);

        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut r = RECT { left: 0, top: 0, right: 360, bottom: 392 };
        let _ = AdjustWindowRect(&mut r, style, FALSE);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("ScreenMirrorCtl"),
            w!("Screen Mirror"),
            style | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            r.right - r.left,
            r.bottom - r.top,
            None,
            None,
            hinst,
            None,
        )?;

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            // Let the dialog handle Tab, Enter and Esc like a real dialog.
            let dlg = OPEN_DLG.with(|d| d.borrow().as_ref().map(|x| x.hwnd));
            let handled = match dlg {
                Some(h) => IsDialogMessageW(h, &msg).as_bool(),
                None => false,
            };
            if !handled {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    Ok(())
}
