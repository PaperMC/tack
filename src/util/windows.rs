/*
 * tack - Paper server launcher with AOT cache management.
 * Copyright (C) 2026  Kyle Wood (DenWav)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, version 3 of the License only.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetConsoleProcessList(lpdw_process_list: *mut u32, dw_process_count: u32) -> u32;
    fn FreeConsole() -> i32;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn MessageBoxW(h_wnd: *mut std::ffi::c_void, lp_text: *const u16, lp_caption: *const u16, u_type: u32) -> i32;
}

// Checks if tack was launched directly by double-clicking in File Explorer (or without a terminal session).
// If so, detaches the console and displays a GUI dialog indicating that tack must be run from the command line.
pub fn check_gui_launch() {
    let mut pids = [0u32; 2];
    let count = unsafe { GetConsoleProcessList(pids.as_mut_ptr(), 2) };
    if count <= 1 {
        unsafe {
            FreeConsole();
            let title: Vec<u16> = OsStr::new("tack").encode_wide().chain(std::iter::once(0)).collect();
            let message: Vec<u16> = OsStr::new("tack is a command-line application and must be run from the terminal.")
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            const MB_OK: u32 = 0x00000000;
            const MB_ICONWARNING: u32 = 0x00000030;
            MessageBoxW(
                std::ptr::null_mut(),
                message.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONWARNING,
            );
        }
        std::process::exit(1);
    }
}
