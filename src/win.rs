//! Win32 glue: program icons and opening URLs / folders.
use eframe::egui::ColorImage;
use std::{
    io,
    mem::zeroed,
    ptr::{null, null_mut},
    sync::atomic::{AtomicBool, Ordering},
};
use windows_sys::Win32::{
    Foundation::CloseHandle,
    Graphics::Gdi::*,
    System::{
        JobObjects::{AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation, QueryInformationJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION},
        Threading::{CreateProcessW, ResumeThread, Sleep, CREATE_NO_WINDOW, CREATE_SUSPENDED, PROCESS_INFORMATION, STARTUPINFOW},
    },
    Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives},
    System::WindowsProgramming::DRIVE_FIXED,
    UI::{
        Shell::{ExtractIconExW, ShellExecuteW},
        WindowsAndMessaging::*,
    },
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// Local fixed drives, e.g. ["C:\", "D:\", "E:\"] (no USB sticks, DVDs or network shares).
pub fn fixed_drives() -> Vec<std::path::PathBuf> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| format!("{}:\\", (b'A' + i) as char))
        .filter(|d| unsafe { GetDriveTypeW(wide(d).as_ptr()) } == DRIVE_FIXED)
        .map(Into::into)
        .collect()
}

/// Run `cmdline` and wait until it *and every process it started* have exited. Inno / NSIS
/// uninstallers copy themselves to %TEMP% and exit at once; the copy still belongs to the job.
/// Setting `cancel` stops waiting (the uninstaller keeps running).
pub fn run_and_wait(cmdline: &str, cancel: &AtomicBool) -> io::Result<()> {
    unsafe {
        let job = CreateJobObjectW(null(), null());
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let si = STARTUPINFOW { cb: size_of::<STARTUPINFOW>() as u32, ..zeroed() };
        let mut pi: PROCESS_INFORMATION = zeroed();
        let mut cmd = wide(cmdline);
        let flags = CREATE_SUSPENDED | CREATE_NO_WINDOW;
        if CreateProcessW(null(), cmd.as_mut_ptr(), null(), null(), 0, flags, null(), null(), &si, &mut pi) == 0 {
            let e = io::Error::last_os_error();
            CloseHandle(job);
            return Err(e);
        }
        // Suspended until it's in the job, so nothing it spawns can slip out.
        AssignProcessToJobObject(job, pi.hProcess);
        ResumeThread(pi.hThread);
        CloseHandle(pi.hThread);
        CloseHandle(pi.hProcess);
        while !cancel.load(Ordering::Relaxed) {
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = zeroed();
            let size = size_of_val(&info) as u32;
            if QueryInformationJobObject(job, JobObjectBasicAccountingInformation, &mut info as *mut _ as _, size, null_mut()) == 0
                || info.ActiveProcesses == 0
            {
                break;
            }
            Sleep(300);
        }
        CloseHandle(job);
        Ok(())
    }
}

/// Open a URL in the default browser or a folder in Explorer.
pub fn open(target: &str) {
    unsafe {
        ShellExecuteW(null_mut(), wide("open").as_ptr(), wide(target).as_ptr(), null(), null(), SW_SHOWNORMAL);
    }
}

/// DisplayIcon is `path` or `"path",index`; returns the icon as RGBA.
pub fn icon(display_icon: &str) -> Option<ColorImage> {
    let s = display_icon.trim();
    let (path, index) = match s.rsplit_once(',') {
        Some((p, i)) if i.trim().parse::<i32>().is_ok() => (p, i.trim().parse().unwrap()),
        _ => (s, 0),
    };
    let path = wide(path.trim().trim_matches('"'));
    unsafe {
        let mut h: HICON = null_mut();
        if ExtractIconExW(path.as_ptr(), index, &mut h, null_mut(), 1) == 0 || h.is_null() {
            return None;
        }
        let img = to_rgba(h);
        DestroyIcon(h);
        img
    }
}

unsafe fn to_rgba(icon: HICON) -> Option<ColorImage> {
    let mut ii: ICONINFO = zeroed();
    if GetIconInfo(icon, &mut ii) == 0 {
        return None;
    }
    let mut img = None;
    if !ii.hbmColor.is_null() {
        let mut bm: BITMAP = zeroed();
        GetObjectW(ii.hbmColor, size_of::<BITMAP>() as i32, &mut bm as *mut _ as _);
        let (w, h) = (bm.bmWidth, bm.bmHeight);
        let mut bi: BITMAPINFO = zeroed();
        bi.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..zeroed()
        };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let dc = CreateCompatibleDC(null_mut());
        let ok = GetDIBits(dc, ii.hbmColor, 0, h as u32, buf.as_mut_ptr() as _, &mut bi, DIB_RGB_COLORS) != 0;
        DeleteDC(dc);
        if ok && w > 0 && h > 0 {
            // old icons carry no alpha channel at all
            let opaque = buf.chunks(4).all(|p| p[3] == 0);
            for p in buf.chunks_mut(4) {
                p.swap(0, 2);
                if opaque {
                    p[3] = 255;
                }
            }
            img = Some(ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &buf));
        }
        DeleteObject(ii.hbmColor);
    }
    DeleteObject(ii.hbmMask);
    img
}
