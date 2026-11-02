// MoNote（墨记）桌面应用入口（Windows GUI 子系统）。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    monote_lib::run()
}
